// VoiceDumps system-audio capture — a standalone helper process.
//
// The microphone gets us half a meeting. This gets the other half: whatever the
// Mac is *playing* — the other people on the call — captured without a virtual
// audio driver, without a bot joining the meeting, and without asking Zoom or
// Meet for anything.
//
// Why a separate process rather than Rust FFI: the tap is created from
// `CATapDescription`, an Objective-C class, and driven by a CoreAudio IOProc.
// Reaching that from Rust means hand-rolled objc_msgSend against a class whose
// initialisers are the only documented way to build a valid description. Swift
// gets it in a dozen lines and the compiler checks the availability guards for
// us. The app already spawns one Swift helper for the dictation overlay, so the
// pattern — and the build step — costs nothing new.
//
// Why the tap and not ScreenCaptureKit: SCK would work, and it is older
// (macOS 13 vs 14.4), but it asks for *screen recording* permission to capture
// audio. Being in the same TCC bucket as "can read everything on your display"
// is a much bigger ask than this feature deserves, and users are right to
// hesitate over it. `NSAudioCaptureUsageDescription` is its own category and
// says exactly what it does.
//
// Protocol — a single line of JSON on stdout, then raw audio until we stop:
//
//   {"rate":48000.0,"channels":1}\n
//   <little-endian f32 mono samples, forever>
//
// Mono, at 16 kHz, always — which is what the transcription engine takes and
// what the app's mixed recording is written at, so nothing downstream has to
// convert again.
//
// The rate cannot be discovered before the first sample: the tap is silent
// until something is playing. So the header promises 16 kHz up front, the
// drain loop works out what the device is really delivering from the first
// second of audio, and everything is resampled to the promise. When the device
// is already at 16 kHz — which a headset in a call is — nothing is resampled at
// all.
//
// This matters because the declared format lies. Measured on a real call: the
// tap reported 48000 Hz while handing over a steady 16000 samples a second,
// with no error anywhere. Taken at face value, an hour of conversation is
// written at three times speed and transcribes as noise.
//
// Every ten seconds the throughput is reported to stderr, so that number is
// never again something to be inferred.
//
// Exit codes are meaningful, because the app turns them into sentences a person
// can act on:
//
//   0  stopped cleanly
//   2  this Mac is older than macOS 14.4 — no tap API
//   3  the system refused the capture (almost always a denied permission)
//   4  CoreAudio failed for some other reason; stderr says which call

import AppKit
import AudioToolbox
import CoreAudio
import Foundation

// MARK: - Exits

private let exitTooOld: Int32 = 2
private let exitDenied: Int32 = 3
private let exitAudio: Int32 = 4

private func note(_ message: String) {
    FileHandle.standardError.write(Data("[capture] \(message)\n".utf8))
}

private func fail(_ code: Int32, _ message: String) -> Never {
    note(message)
    exit(code)
}

private enum CaptureError: Error {
    /// The system refused us a tap. Nearly always a denied TCC grant.
    case denied(String)
    /// CoreAudio said no for a reason that is not about permission.
    case audio(String)
}

/// Turn an OSStatus into something readable. CoreAudio codes are four-character
/// literals more often than they are numbers, and printing the raw Int32 turns
/// a diagnosable failure into a support ticket.
private func fourCC(_ status: OSStatus) -> String {
    let raw = UInt32(bitPattern: status)
    let bytes = [
        UInt8((raw >> 24) & 0xFF), UInt8((raw >> 16) & 0xFF),
        UInt8((raw >> 8) & 0xFF), UInt8(raw & 0xFF),
    ]
    if bytes.allSatisfy({ $0 >= 0x20 && $0 < 0x7F }) {
        return "'\(String(decoding: bytes, as: UTF8.self))' (\(status))"
    }
    return "\(status)"
}

/// Write every byte or report why not. `FileHandle.write` raises an ObjC
/// exception on a closed pipe that Swift cannot catch, which would turn "the app
/// stopped listening" into a crash report; the raw syscall just returns EPIPE
/// and lets us exit quietly.
@discardableResult
private func writeAll(_ bytes: UnsafeRawBufferPointer) -> Bool {
    guard let base = bytes.baseAddress else { return true }
    var offset = 0
    while offset < bytes.count {
        let written = write(1, base.advanced(by: offset), bytes.count - offset)
        if written > 0 {
            offset += written
            continue
        }
        if written == -1 && errno == EINTR { continue }
        return false
    }
    return true
}

// MARK: - The outgoing byte stream
//
// The IOProc runs on a realtime thread, so it does the least it can: convert to
// mono and hand the samples off. A drain loop does the actual `write(2)`, which
// can block for as long as the reader is busy and must never do so on the audio
// thread.
//
// This mirrors what the app's own microphone capture already does — it takes a
// lock in the cpal callback and appends — so the contention profile here is a
// known quantity rather than a new bet.

private final class Outbox: @unchecked Sendable {
    private let lock = NSLock()
    private var pending = [Float]()
    private var closed = false
    private var overflowed = false

    /// Roughly ten seconds at 48 kHz. A bound exists so that a stalled reader
    /// costs bounded memory instead of the machine; dropping the oldest audio
    /// is the correct casualty, since by then the recording is already ruined
    /// and the alternative is taking the app down with it.
    private let limit = 480_000

    func push(_ samples: [Float]) {
        lock.lock()
        defer { lock.unlock() }
        guard !closed else { return }
        pending.append(contentsOf: samples)
        if pending.count > limit {
            pending.removeFirst(pending.count - limit)
            overflowed = true
        }
    }

    /// `nil` once the producer is finished and everything buffered has been
    /// handed over — the drain loop's signal to stop.
    func drain() -> [Float]? {
        lock.lock()
        defer { lock.unlock() }
        if pending.isEmpty { return closed ? nil : [] }
        let out = pending
        pending.removeAll(keepingCapacity: true)
        return out
    }

    func close() {
        lock.lock()
        closed = true
        lock.unlock()
    }

    /// How many samples are waiting, without taking them.
    ///
    /// Used to measure the incoming rate before the drain loop starts, which is
    /// why it has to be a peek: the audio counted here is still delivered.
    var count: Int {
        lock.lock()
        defer { lock.unlock() }
        return pending.count
    }

    var didOverflow: Bool {
        lock.lock()
        defer { lock.unlock() }
        return overflowed
    }
}

// MARK: - Capture

@available(macOS 14.4, *)
private final class SystemAudioCapture: @unchecked Sendable {
    private var tap = AudioObjectID(kAudioObjectUnknown)
    private var aggregate = AudioObjectID(kAudioObjectUnknown)
    private var procID: AudioDeviceIOProcID?
    private let outbox = Outbox()
    /// The rate written into the header, and therefore into the reader's WAV.
    /// Everything sent afterwards has to actually be at this rate.
    private var announcedRate: Double = 0
    /// Samples handed over since the last throughput report, and when that was.
    private var delivered = 0
    private var reportedAt: TimeInterval = 0
    /// The rate the tap is really running at, once two measurements have
    /// agreed. Zero means "still watching, and passing audio through as-is".
    private var sourceRate: Double = 0
    /// Samples actually written out since the last report.
    private var emitted = 0
    /// The measurement window in progress, and the answer the last one gave.
    private var windowSamples = 0
    private var windowStarted: TimeInterval = 0
    private var previousWindow: Double = 0
    /// Sub-sample phase carried between batches so the seams do not click.
    private var carry: Double = 0

    private let lock = NSLock()
    private var running = false

    // MARK: Device lookups

    /// Deliberately concrete rather than generic over the property type: a
    /// generic `T` here makes the compiler warn that the out-pointer might be
    /// handed something containing an object reference, which for a CFType
    /// property would be a real over-release waiting to happen. The only plain
    /// values we read are device IDs, and `deviceUID` below handles the one
    /// CFType case with the ownership spelled out.
    private static func deviceID(
        _ object: AudioObjectID,
        _ selector: AudioObjectPropertySelector
    ) -> AudioObjectID? {
        var address = AudioObjectPropertyAddress(
            mSelector: selector,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        var value = AudioObjectID(kAudioObjectUnknown)
        var size = UInt32(MemoryLayout<AudioObjectID>.size)
        let status = AudioObjectGetPropertyData(object, &address, 0, nil, &size, &value)
        guard status == noErr else { return nil }
        return value
    }

    /// The device the user is actually listening on. The tap follows the
    /// *default output* rather than a device we pick, so plugging in headphones
    /// before a call does not silently capture laptop speakers nobody is using.
    private static func defaultOutputDevice() -> AudioObjectID? {
        let id = deviceID(
            AudioObjectID(kAudioObjectSystemObject),
            kAudioHardwarePropertyDefaultOutputDevice
        )
        guard let id, id != kAudioObjectUnknown else { return nil }
        return id
    }

    private static func deviceUID(_ device: AudioObjectID) -> String? {
        var address = AudioObjectPropertyAddress(
            mSelector: kAudioDevicePropertyDeviceUID,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        // `Unmanaged` rather than a plain `CFString?` so the ownership is
        // written down instead of guessed at. This property hands back a
        // retained string — CoreAudio's convention for a "Copy"-style get — so
        // the balancing release is ours, and `takeRetainedValue` is what
        // performs it.
        var uid: Unmanaged<CFString>?
        var size = UInt32(MemoryLayout<Unmanaged<CFString>?>.size)
        let status = withUnsafeMutablePointer(to: &uid) {
            AudioObjectGetPropertyData(device, &address, 0, nil, &size, $0)
        }
        guard status == noErr, let uid else { return nil }
        return uid.takeRetainedValue() as String
    }

    // MARK: Lifecycle

    func start() throws {
        guard let output = Self.defaultOutputDevice(), let outputUID = Self.deviceUID(output)
        else {
            throw CaptureError.audio("no audio output device to capture")
        }

        // Tap everything, excluding nothing. The alternative — naming the
        // processes we want — would mean maintaining a list of every
        // conferencing app that exists, and quietly capturing nothing the day
        // someone uses one we have not heard of.
        let description = CATapDescription(stereoGlobalTapButExcludeProcesses: [])
        description.uuid = UUID()
        description.name = "VoiceDumps meeting capture"
        // Private: the tap is ours and should not appear in other apps' pickers.
        description.isPrivate = true
        // The single most important line in this file. A tap can mute what it
        // captures, and muting the output would take the other side of the call
        // away from the user's own ears the moment we start recording.
        // `.unmuted` means they keep hearing the meeting and we get a copy.
        description.muteBehavior = .unmuted

        let tapStatus = AudioHardwareCreateProcessTap(description, &tap)
        guard tapStatus == noErr, tap != kAudioObjectUnknown else {
            // A refusal here is nearly always TCC: the user has not granted (or
            // has revoked) audio capture in System Settings.
            throw CaptureError.denied(
                "the system would not create an audio tap: \(fourCC(tapStatus))")
        }

        // The tap only produces audio once it belongs to a running aggregate
        // device. Marked private so it does not become a selectable input for
        // other apps, and drift compensation is on because the tap and the
        // output device are separate clocks that will otherwise walk apart over
        // the length of a call.
        let aggregateDescription: [String: Any] = [
            kAudioAggregateDeviceNameKey: "VoiceDumps Meeting Capture",
            kAudioAggregateDeviceUIDKey: UUID().uuidString,
            kAudioAggregateDeviceMainSubDeviceKey: outputUID,
            kAudioAggregateDeviceIsPrivateKey: true,
            kAudioAggregateDeviceIsStackedKey: false,
            kAudioAggregateDeviceTapAutoStartKey: true,
            kAudioAggregateDeviceSubDeviceListKey: [[kAudioSubDeviceUIDKey: outputUID]],
            kAudioAggregateDeviceTapListKey: [
                [
                    kAudioSubTapDriftCompensationKey: true,
                    kAudioSubTapUIDKey: description.uuid.uuidString,
                ]
            ],
        ]

        let aggregateStatus = AudioHardwareCreateAggregateDevice(
            aggregateDescription as CFDictionary, &aggregate)
        guard aggregateStatus == noErr, aggregate != kAudioObjectUnknown else {
            teardown()
            throw CaptureError.audio(
                "could not create the capture device: \(fourCC(aggregateStatus))")
        }

        var format = AudioStreamBasicDescription()
        var formatSize = UInt32(MemoryLayout<AudioStreamBasicDescription>.size)
        var formatAddress = AudioObjectPropertyAddress(
            mSelector: kAudioTapPropertyFormat,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        let formatStatus = AudioObjectGetPropertyData(
            tap, &formatAddress, 0, nil, &formatSize, &format)
        guard formatStatus == noErr, format.mSampleRate > 0 else {
            teardown()
            throw CaptureError.audio(
                "could not read the tap's audio format: \(fourCC(formatStatus))")
        }

        guard format.mFormatID == kAudioFormatLinearPCM,
            format.mFormatFlags & kAudioFormatFlagIsFloat != 0,
            format.mBitsPerChannel == 32,
            format.mChannelsPerFrame > 0
        else {
            teardown()
            throw CaptureError.audio(
                "unexpected tap format: \(format.mBitsPerChannel)-bit, "
                    + "\(format.mChannelsPerFrame)ch, flags \(format.mFormatFlags)")
        }
        let interleaved = format.mFormatFlags & kAudioFormatFlagIsNonInterleaved == 0

        note(
            "tap format: \(format.mSampleRate) Hz, \(format.mChannelsPerFrame)ch, "
                + "\(format.mBytesPerFrame) bytes/frame, flags \(format.mFormatFlags), "
                + "interleaved \(interleaved)")

        let outbox = self.outbox
        // What the tap *says* and what the aggregate device *delivers* are two
        // different questions, and answering the second from the first is how
        // audio ends up sounding robotic: a stereo buffer de-interleaved with
        // the wrong geometry is still audio, just not the audio that went in.
        // Described once, from the first buffer that actually arrives.
        var described = false
        let ioStatus = AudioDeviceCreateIOProcIDWithBlock(&procID, aggregate, nil) {
            _, inInputData, _, _, _ in
            let buffers = UnsafeMutableAudioBufferListPointer(
                UnsafeMutablePointer(mutating: inInputData))
            guard buffers.count > 0 else { return }
            if !described {
                described = true
                let shape = buffers.map { "\($0.mNumberChannels)ch/\($0.mDataByteSize)B" }
                note("first buffer: \(buffers.count) buffer(s) — \(shape.joined(separator: ", "))")
            }
            outbox.push(Self.mono(from: buffers, interleaved: interleaved))
        }

        guard ioStatus == noErr, let proc = procID else {
            teardown()
            throw CaptureError.audio(
                "could not attach to the capture device: \(fourCC(ioStatus))")
        }

        let startStatus = AudioDeviceStart(aggregate, proc)
        guard startStatus == noErr else {
            AudioDeviceDestroyIOProcID(aggregate, proc)
            procID = nil
            teardown()
            throw CaptureError.audio("could not start capturing: \(fourCC(startStatus))")
        }

        lock.lock()
        running = true
        lock.unlock()

        // The rate is announced now and honoured later — and it is the rate
        // the app actually wants, not the one the device happens to run at.
        //
        // It cannot be measured here: the tap sends nothing at all until
        // someone speaks, so any window opened at startup measures silence. An
        // earlier version waited 1.2 seconds, caught nothing, and fell back to
        // the declared rate, which was three times too high.
        //
        // 16 kHz because that is where this audio is going regardless. The
        // transcription engine takes 16 kHz, the mixed recording the app
        // archives is written at 16 kHz, and the far side's native rate is
        // never kept by anything. Announcing 48 kHz meant synthesising three
        // samples where the device gave one, and the app then throwing two of
        // them away again — two lossy passes, neither with a filter, which is
        // audible as a thin metallic edge on every voice. Sending what is
        // wanted means the common case resamples nothing at all.
        announcedRate = 16000
        let header = "{\"rate\":16000.0,\"channels\":1}\n"
        guard Array(header.utf8).withUnsafeBytes({ writeAll($0) }) else {
            teardown()
            throw CaptureError.audio("the app closed the pipe before we started")
        }
        note("capturing from \(outputUID), sending 16000 Hz")
    }


    /// The nearest rate anyone actually ships, or the raw figure if none is
    /// close. A measurement carries the jitter of however many buffers landed
    /// inside the window, and a WAV header wants a round number.
    private func snapToStandardRate(_ observed: Double) -> Double {
        let standard: [Double] = [8000, 16000, 22050, 24000, 32000, 44100, 48000, 88200, 96000]
        guard let closest = standard.min(by: { abs($0 - observed) < abs($1 - observed) })
        else { return observed }
        return abs(closest - observed) / closest < 0.15 ? closest : observed
    }

    /// Resample to the rate promised in the header.
    ///
    /// The promise was made before a single sample arrived, because the tap is
    /// silent until someone speaks and there is nothing to measure at startup.
    /// Keeping it is what stops an hour of conversation being filed at three
    /// times speed — which is what happened when the declared rate was taken at
    /// face value, with no error anywhere to say so.
    private func conform(_ samples: [Float]) -> [Float] {
        guard sourceRate > 0, announcedRate > 0, abs(sourceRate - announcedRate) > 1
        else { return samples }

        let ratio = announcedRate / sourceRate
        var out = [Float]()
        out.reserveCapacity(Int(Double(samples.count) * ratio) + 2)
        var at = carry
        while at < Double(samples.count) {
            let j = Int(at)
            let frac = Float(at - Double(j))
            let a = samples[j]
            let b = j + 1 < samples.count ? samples[j + 1] : a
            out.append(a + (b - a) * frac)
            at += 1 / ratio
        }
        // Whatever fraction of a source sample this batch ended part-way
        // through, carried into the next one. Without it every batch restarts
        // the phase and the joins click.
        carry = at - Double(samples.count)
        return out
    }

    /// Down to mono, because that is what speech recognition wants and stereo
    /// doubles the bytes to say the same thing. Both layouts appear in the wild
    /// depending on the output device, so handle each rather than assuming.
    private static func mono(
        from buffers: UnsafeMutableAudioBufferListPointer, interleaved: Bool
    ) -> [Float] {
        if interleaved {
            let buffer = buffers[0]
            guard let data = buffer.mData else { return [] }
            let channels = max(Int(buffer.mNumberChannels), 1)
            let frames = Int(buffer.mDataByteSize) / (MemoryLayout<Float>.size * channels)
            guard frames > 0 else { return [] }
            let samples = data.assumingMemoryBound(to: Float.self)
            var mono = [Float]()
            mono.reserveCapacity(frames)
            for frame in 0..<frames {
                var sum: Float = 0
                for channel in 0..<channels {
                    sum += samples[frame * channels + channel]
                }
                mono.append(sum / Float(channels))
            }
            return mono
        }

        let frames = Int(buffers[0].mDataByteSize) / MemoryLayout<Float>.size
        guard frames > 0 else { return [] }
        var mono = [Float](repeating: 0, count: frames)
        var contributing = 0
        for buffer in buffers {
            guard let data = buffer.mData else { continue }
            let samples = data.assumingMemoryBound(to: Float.self)
            let available = min(frames, Int(buffer.mDataByteSize) / MemoryLayout<Float>.size)
            for frame in 0..<available {
                mono[frame] += samples[frame]
            }
            contributing += 1
        }
        if contributing > 1 {
            let scale = 1 / Float(contributing)
            for frame in 0..<frames { mono[frame] *= scale }
        }
        return mono
    }

    /// Tear down in the reverse order we built, every time. A leaked aggregate
    /// device survives the process and shows up in Audio MIDI Setup, which is
    /// both untidy and, after enough crashed runs, an actual problem.
    private func teardown() {
        if aggregate != kAudioObjectUnknown {
            AudioHardwareDestroyAggregateDevice(aggregate)
            aggregate = AudioObjectID(kAudioObjectUnknown)
        }
        if tap != kAudioObjectUnknown {
            AudioHardwareDestroyProcessTap(tap)
            tap = AudioObjectID(kAudioObjectUnknown)
        }
    }

    func stop() {
        lock.lock()
        guard running else {
            lock.unlock()
            return
        }
        running = false
        lock.unlock()

        if let proc = procID {
            AudioDeviceStop(aggregate, proc)
            AudioDeviceDestroyIOProcID(aggregate, proc)
            procID = nil
        }
        teardown()
        outbox.close()
    }

    private var isRunning: Bool {
        lock.lock()
        defer { lock.unlock() }
        return running
    }

    /// Own the calling thread until capture ends, forwarding audio as it lands.
    func drainUntilFinished() {
        while true {
            guard let samples = outbox.drain() else { break }
            if !samples.isEmpty {
                // How many samples per second are *actually* arriving, against
                // the rate we announced. Everything downstream — the WAV
                // header, the alignment between the two tracks, the resampler
                // — trusts that number, and nothing has ever checked it. A tap
                // that pauses during silence and one that runs at a third of
                // the announced rate look identical from here without it.
                delivered += samples.count
                let now = Date().timeIntervalSince1970
                if reportedAt == 0 { reportedAt = now }
                if now - reportedAt >= 10 {
                    let window = now - reportedAt
                    let observed = Double(delivered) / window
                    let sending = Double(emitted) / window
                    // Both ends, because the input rate alone is reassuring
                    // even when the output is wrong: a stream arriving at
                    // 16000 reads as a perfect ratio while being resampled by
                    // a mistaken factor on its way out. The number that has to
                    // match the header is the one being sent.
                    note(
                        String(
                            format:
                                "in %.0f Hz, out %.0f Hz over %.0fs "
                                + "(header says %.0f Hz — out/header %.2f)",
                            observed, sending, window, announcedRate,
                            announcedRate > 0 ? sending / announcedRate : 0))
                    delivered = 0
                    emitted = 0
                    reportedAt = now
                }
                // Until the real rate is known, hold on to everything: audio
                // emitted at the wrong rate cannot be corrected downstream,
                // and a second of latency at the start of a call costs
                // nothing. Once known, the backlog goes out conformed and
                // every batch after it follows.
                // Pass straight through until there is *stable* evidence
                // that the device is running at some other rate.
                //
                // This is deliberately biased toward doing nothing. Two
                // attempts at deciding quickly both decided wrong — 22050 Hz
                // and then 24000 Hz, for a stream that was 16000 both times —
                // and each one resampled a correct stream by a wrong ratio for
                // an entire call. A missed correction costs the rate being
                // wrong, which is what it already was; a wrong correction
                // takes audio that was right and breaks it. Those are not the
                // same mistake and they should not be equally easy to make.
                //
                // The device bursts as it starts, so any short window near the
                // beginning reads high. Two consecutive five-second windows
                // have to agree with each other before their answer is acted
                // on at all.
                if sourceRate == 0 {
                    if windowStarted == 0 { windowStarted = now }
                    windowSamples += samples.count
                    let span = now - windowStarted
                    if span >= 5.0, windowSamples >= 2000 {
                        let observed = Double(windowSamples) / span
                        // Agreeing to within 3% means neither window caught a
                        // burst or a gap; anything looser and 16000 and 16500
                        // count as the same reading when they snap apart.
                        if previousWindow > 0,
                            abs(observed - previousWindow) / previousWindow < 0.03
                        {
                            let settled = snapToStandardRate((observed + previousWindow) / 2)
                            if abs(settled - announcedRate) / announcedRate > 0.05 {
                                sourceRate = settled
                                note(
                                    "two windows agree the tap is delivering "
                                        + "\(Int(settled)) Hz, not the \(Int(announcedRate)) Hz "
                                        + "declared; conforming from here on")
                            } else {
                                sourceRate = announcedRate
                                note(
                                    "the tap is delivering \(Int(settled)) Hz, as declared; "
                                        + "nothing to correct")
                            }
                        }
                        previousWindow = observed
                        windowSamples = 0
                        windowStarted = now
                    }
                }

                let out = conform(samples)
                if !out.isEmpty {
                    emitted += out.count
                    let sent = out.withUnsafeBytes { writeAll($0) }
                    if !sent { break }
                }
            } else if !isRunning {
                break
            }
            // 20 ms: fine-grained enough that the app's level meter feels live,
            // coarse enough that we are not waking the CPU for a few samples.
            usleep(20_000)
        }
        stop()
        if outbox.didOverflow {
            note("the reader fell behind and some audio was dropped")
        }
    }

}

// MARK: - Watching for a meeting
//
// `--watch-input` is the other half of the feature: it reports which apps are
// *using the microphone*, which is the closest thing macOS offers to "a call is
// happening". Nothing is recorded here and no audio is read — only the fact that
// some other process has the input device open.
//
// Why this signal rather than a calendar: a calendar knows about the meetings
// someone bothered to schedule, and misses every call that starts with "got a
// minute?". It also costs a whole separate permission and an account to connect.
// The microphone is a fact about right now, needs nothing configured, and is
// equally true for Zoom, Meet in a browser, Teams, FaceTime and a phone held up
// to the laptop.
//
// Output is one JSON line per change, so the app can react rather than poll:
//
//   {"event":"started","bundle":"us.zoom.xos","name":"zoom.us"}
//   {"event":"stopped","bundle":"us.zoom.xos","name":"zoom.us"}

@available(macOS 14.4, *)
private enum InputWatcher {
    /// Every process CoreAudio knows about, whether or not it is making noise.
    static func processObjects() -> [AudioObjectID] {
        var address = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyProcessObjectList,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        var size: UInt32 = 0
        let sized = AudioObjectGetPropertyDataSize(
            AudioObjectID(kAudioObjectSystemObject), &address, 0, nil, &size)
        guard sized == noErr, size > 0 else { return [] }

        let count = Int(size) / MemoryLayout<AudioObjectID>.size
        var objects = [AudioObjectID](repeating: AudioObjectID(kAudioObjectUnknown), count: count)
        let read = AudioObjectGetPropertyData(
            AudioObjectID(kAudioObjectSystemObject), &address, 0, nil, &size, &objects)
        guard read == noErr else { return [] }
        return objects
    }

    /// Is this process recording right now?
    static func isRunningInput(_ object: AudioObjectID) -> Bool {
        var address = AudioObjectPropertyAddress(
            mSelector: kAudioProcessPropertyIsRunningInput,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        var running: UInt32 = 0
        var size = UInt32(MemoryLayout<UInt32>.size)
        let status = AudioObjectGetPropertyData(object, &address, 0, nil, &size, &running)
        return status == noErr && running != 0
    }

    static func bundleID(_ object: AudioObjectID) -> String? {
        var address = AudioObjectPropertyAddress(
            mSelector: kAudioProcessPropertyBundleID,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        // Same ownership reasoning as `deviceUID` above: a copied CFString is
        // ours to release, and `Unmanaged` is where that gets written down.
        var value: Unmanaged<CFString>?
        var size = UInt32(MemoryLayout<Unmanaged<CFString>?>.size)
        let status = withUnsafeMutablePointer(to: &value) {
            AudioObjectGetPropertyData(object, &address, 0, nil, &size, $0)
        }
        guard status == noErr, let value else { return nil }
        let bundle = value.takeRetainedValue() as String
        return bundle.isEmpty ? nil : bundle
    }

    static func pid(_ object: AudioObjectID) -> pid_t? {
        var address = AudioObjectPropertyAddress(
            mSelector: kAudioProcessPropertyPID,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        var value: pid_t = 0
        var size = UInt32(MemoryLayout<pid_t>.size)
        let status = AudioObjectGetPropertyData(object, &address, 0, nil, &size, &value)
        guard status == noErr, value > 0 else { return nil }
        return value
    }

    /// What a person calls this app. The bundle identifier is what we match on,
    /// but "us.zoom.xos" is not a thing to put in a notification.
    /// Whoever spawned this process, or `nil` at the top of the tree.
    ///
    /// `sysctl` rather than parsing `ps`: it is the same answer without a
    /// subprocess, and this runs in a poll loop.
    static func parentPID(of pid: pid_t) -> pid_t? {
        var name: [Int32] = [CTL_KERN, KERN_PROC, KERN_PROC_PID, pid]
        var info = kinfo_proc()
        var size = MemoryLayout<kinfo_proc>.size
        let status = sysctl(&name, UInt32(name.count), &info, &size, nil, 0)
        guard status == 0, size > 0 else { return nil }
        let parent = info.kp_eproc.e_ppid
        return parent > 1 ? parent : nil
    }

    /// The name a person would recognise for whoever is holding the microphone.
    ///
    /// The process with the device open is usually not the app: browsers put it
    /// in a helper, and a helper is named for itself — "Browser Helper", never
    /// "Dia". Nobody joined a call with Browser Helper. Walking up the process
    /// tree finds the app that owns the helper, which is the one to name.
    ///
    /// Bundle-name arithmetic was the first attempt and it does not survive
    /// contact: Dia's helper is `company.thebrowser.browser.helper` while the
    /// app itself is `company.thebrowser.dia`, so no amount of trimming gets
    /// from one to the other. Parentage is a fact; string surgery was a guess.
    static func displayName(bundle: String, pid: pid_t?) -> String {
        var current = pid
        // Three levels covers helper → renderer → browser. Bounded because a
        // cycle here would hang the watcher, and nothing legitimate is deeper.
        for _ in 0..<3 {
            guard let candidate = current else { break }
            if let app = NSRunningApplication(processIdentifier: candidate),
                let name = app.localizedName, !name.isEmpty
            {
                return name
            }
            current = parentPID(of: candidate)
        }
        // Reversed-DNS tail is a decent last resort: "com.hnc.Discord" → "Discord".
        return bundle.split(separator: ".").last.map(String.init) ?? bundle
    }
}

/// Report microphone use until the app stops listening.
@available(macOS 14.4, *)
private func runWatcher() -> Never {
    Thread.detachNewThread {
        var byte: UInt8 = 0
        while read(0, &byte, 1) > 0 {}
        // The app is gone; there is nobody to report to.
        exit(0)
    }

    // Keyed by the process object, not by bundle: a bundle can have several
    // (browsers run one helper per tab) and they come and go independently.
    var active: [AudioObjectID: (bundle: String, name: String)] = [:]

    func emit(_ event: String, _ bundle: String, _ name: String) {
        // Hand-built rather than JSONEncoder: two fields, and escaping the app
        // name is the only part that needs care. A name with a quote in it
        // would otherwise produce a line the reader cannot parse.
        let escape = { (s: String) -> String in
            s.replacingOccurrences(of: "\\", with: "\\\\")
                .replacingOccurrences(of: "\"", with: "\\\"")
        }
        let line = """
            {"event":"\(event)","bundle":"\(escape(bundle))","name":"\(escape(name))"}

            """
        guard Array(line.utf8).withUnsafeBytes({ writeAll($0) }) else { exit(0) }
    }

    while true {
        var current: [AudioObjectID: (bundle: String, name: String)] = [:]
        for object in InputWatcher.processObjects() where InputWatcher.isRunningInput(object) {
            // A process with no bundle identifier is reported anyway, with an
            // empty one. Deciding that a nameless command-line tool holding the
            // microphone is not a meeting is policy, and policy lives in the
            // app where it can be tested — not in the thing doing the looking.
            let bundle = InputWatcher.bundleID(object) ?? ""
            current[object] = (
                bundle,
                InputWatcher.displayName(bundle: bundle, pid: InputWatcher.pid(object))
            )
        }

        for (object, who) in current where active[object] == nil {
            emit("started", who.bundle, who.name)
        }
        for (object, who) in active where current[object] == nil {
            emit("stopped", who.bundle, who.name)
        }
        active = current

        // A second and a half. Nobody notices that much delay before being
        // offered notes, and polling this often costs nothing measurable —
        // whereas subscribing to per-process listeners means re-registering
        // every time an app launches, for the same answer.
        usleep(1_500_000)
    }
}

// MARK: - Entry
//
// We stop when the app stops us, and the app stops us by closing our stdin —
// which happens on a clean quit and equally on a crash. Watching the pipe rather
// than waiting for a signal means an orphaned helper cannot outlive the app and
// keep a tap open on the user's audio, which is the one failure mode of this
// feature that would genuinely deserve a bug report.

// Everything the tap touches lives behind one availability wall. `guard
// #available` does not narrow the statements that follow it at file scope, so
// the whole entry sequence is a function the compiler can annotate instead.
@available(macOS 14.4, *)
private func runCapture() -> Never {
    let capture = SystemAudioCapture()

    do {
        try capture.start()
    } catch CaptureError.denied(let why) {
        fail(exitDenied, "\(why) — this is usually a denied audio-recording permission")
    } catch CaptureError.audio(let why) {
        fail(exitAudio, why)
    } catch {
        fail(exitAudio, "\(error)")
    }

    // Retained for the life of the process: a deallocated source stops
    // delivering.
    var signalSources = [DispatchSourceSignal]()
    for signalNumber in [SIGTERM, SIGINT, SIGHUP] {
        // Ignore the default disposition first, or the process dies before the
        // dispatch source ever sees the signal.
        signal(signalNumber, SIG_IGN)
        // Deliberately *not* the main queue: the drain loop below owns the main
        // thread and never returns to a run loop, so a handler scheduled there
        // would never fire and the helper would ignore every signal sent to it.
        let source = DispatchSource.makeSignalSource(
            signal: signalNumber, queue: .global(qos: .userInitiated))
        source.setEventHandler { capture.stop() }
        source.resume()
        signalSources.append(source)
    }

    Thread.detachNewThread {
        // Any input at all is a stop request, but EOF is the one that matters.
        var byte: UInt8 = 0
        while read(0, &byte, 1) > 0 {}
        capture.stop()
    }

    capture.drainUntilFinished()
    withExtendedLifetime(signalSources) {}
    exit(0)
}

if #available(macOS 14.4, *) {
    if CommandLine.arguments.contains("--watch-input") {
        runWatcher()
    } else {
        runCapture()
    }
} else {
    fail(
        exitTooOld,
        "system audio capture needs macOS 14.4 or later; this Mac is running "
            + ProcessInfo.processInfo.operatingSystemVersionString
    )
}
