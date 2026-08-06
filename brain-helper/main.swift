// VoiceDumps on-device language model — a standalone helper process.
//
// The full build sends a transcript to Bedrock to get an overview back. That
// costs money per note and sends the note somewhere, which makes it exactly the
// wrong answer for the public build: the whole promise there is that nothing
// leaves the Mac and there is nothing to sign in to.
//
// macOS 26 ships the model Apple Intelligence itself runs on, and lets any app
// use it: no key, no per-token bill, no network. The weights are owned by the
// OS and shared with every other feature using them, so an app that calls it
// does not carry the model in its own memory — which is the only reason a
// 3-billion-parameter model is thinkable in a 25 MB app that already asks the
// user to download 695 MB of speech weights.
//
// Why a separate process rather than Rust FFI: `FoundationModels` is
// Swift-only — not merely Objective-C with a Swift veneer, but an API whose
// surface is async/await, actors and result builders. There is no C entry point
// to bind against. The app already spawns two Swift helpers, so a third costs a
// build step and nothing else.
//
// Why no `@Generable`: the framework's guided-generation macro would remove the
// JSON parsing below entirely, and it is the nicer API. Its implementation ships
// inside Xcode, not in the Command Line Tools, so depending on it would turn a
// working `swiftc` build into a 10 GB Xcode prerequisite for anyone building
// this repo.
//
// That is a statement about the *macro*, not about guided generation itself.
// `DynamicGenerationSchema` builds the same constraint at runtime out of plain
// values, compiles under `swiftc` alone, and is the single most important thing
// this helper does — see `--schema` below.
//
// Protocol — one JSON job per line on stdin, one JSON answer per line on stdout:
//
//   {"id":1,"instructions":"...","prompt":"...","max_tokens":600}\n
//   -> {"id":1,"ok":true,"text":"..."}\n
//   -> {"id":1,"ok":false,"error":"...","overflow":true}\n
//
// `overflow` is set when the job did not fit the context window, so the caller
// can split its input and try again rather than reporting a dead end.
//
// A job may also carry a `schema`, which changes the answer from prose to JSON
// that is guaranteed to fit the shape asked for:
//
//   {"id":1,...,"schema":{"name":"Answer","fields":[
//       {"name":"text","desc":"The answer.","type":"string"}]}}\n
//   -> {"id":1,"ok":true,"json":"{\"text\":\"...\"}"}\n
//
// This is not a convenience. Constrained decoding masks out every token that
// would not continue a valid instance of the schema, and the tokens the model
// uses to call a tool are among them — so under a schema it *cannot* emit the
// tool call that otherwise ends the turn with no answer at all. Measured on the
// prompt shape the chat feature actually builds: 0/5 plain, 0/5 with the notes
// fenced off in the prompt, 5/5 under a schema. Prose generation is the
// unreliable path here, not the fancy one.
//
// And `op`, which decides whether the model remembers anything:
//
//   (absent) or "once"  a fresh session, used and discarded  — the default
//   "graft"             replace the kept session with one built from `history`
//   "ask"               use the kept session, and add this turn to it
//
//   {"id":1,"op":"graft","instructions":"...","history":[["question","answer"]]}\n
//   -> {"id":1,"ok":true,"entries":3}\n
//
// Grafting exists because the two obvious ways to give a chat a memory both
// fail. Letting one session accumulate works — it answers "write that as a
// paragraph" correctly — but every turn re-sends every note it was ever given,
// and it dies at the seventh question. Re-rendering the history into the next
// prompt keeps it small, but the model reads "earlier in this conversation" as
// material to act on and goes back to emitting tool calls. Building the
// transcript by hand avoids both: the caller keeps the questions and the
// answers, drops the bulky retrieved notes, and hands back a memory that is
// small, durable across restarts, and genuinely the model's own.
//
// `--check` prints one line of JSON describing whether this Mac can run any of
// it, and exits 0 either way. The answer is data, not an exit code, because
// "your Mac is fine but Apple Intelligence is switched off" is a sentence with a
// fix in it and the app should be able to say so.
//
// Exit codes match the capture helper, so the app reads both the same way:
//
//   0  finished cleanly
//   2  this Mac is older than macOS 26 — no on-device model
//   3  the model exists but is unavailable (switched off, or still downloading)
//   4  something else failed; stderr says what

import Foundation

#if canImport(FoundationModels)
    import FoundationModels
#endif

// MARK: - Exits

private let exitTooOld: Int32 = 2
private let exitUnavailable: Int32 = 3
private let exitFailed: Int32 = 4

private func note(_ message: String) {
    FileHandle.standardError.write(Data("[brain] \(message)\n".utf8))
}

/// Write one line of JSON to stdout and flush it.
///
/// Flushing matters: the app reads these as they arrive to drive a progress bar
/// through a summary that takes tens of seconds, and a buffered stdout would
/// deliver the whole conversation at the end, when nobody needs it any more.
private func emit(_ object: [String: Any]) {
    guard let data = try? JSONSerialization.data(withJSONObject: object) else { return }
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write(Data("\n".utf8))
}

// MARK: - Availability

/// The reasons, as stable slugs.
///
/// Spelled out here rather than passing the enum's description through, because
/// the app turns each one into a different sentence and a framework release that
/// reworded its own description should not silently change what the user reads.
private enum Verdict {
    static let ok = "available"
    static let tooOld = "os-too-old"
    static let notEligible = "device-not-eligible"
    static let notEnabled = "apple-intelligence-off"
    static let notReady = "model-not-ready"
    static let unknown = "unavailable"
}

#if canImport(FoundationModels)
    @available(macOS 26.0, *)
    private func verdict() -> String {
        switch SystemLanguageModel.default.availability {
        case .available:
            return Verdict.ok
        case .unavailable(let reason):
            switch reason {
            case .deviceNotEligible: return Verdict.notEligible
            case .appleIntelligenceNotEnabled: return Verdict.notEnabled
            case .modelNotReady: return Verdict.notReady
            @unknown default: return Verdict.unknown
            }
        @unknown default:
            return Verdict.unknown
        }
    }
#endif

/// What this Mac can do, as one line of JSON.
private func check() -> Never {
    #if canImport(FoundationModels)
        if #available(macOS 26.0, *) {
            let answer = verdict()
            emit(["available": answer == Verdict.ok, "reason": answer])
            exit(0)
        }
    #endif
    emit(["available": false, "reason": Verdict.tooOld])
    exit(0)
}

// MARK: - Jobs

/// What the caller wants done with the session.
private enum Op: String {
    /// A fresh session, used once and thrown away.
    case once
    /// Replace the kept session with one built from a supplied history.
    case graft
    /// Use the kept session, and let this turn join it.
    case ask
}

private struct Job {
    let id: Int
    let op: Op
    let instructions: String
    let prompt: String
    let maxTokens: Int
    /// A shape the answer must take, or nil to let the model write prose.
    let schema: [String: Any]?
    /// Question-and-answer pairs to build a memory from, for `graft`.
    let history: [[String]]
}

private func parse(_ line: String) -> Job? {
    guard
        let data = line.data(using: .utf8),
        let raw = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
        let id = raw["id"] as? Int
    else { return nil }
    let op = Op(rawValue: raw["op"] as? String ?? "") ?? .once
    // Every op but `graft` needs something to answer.
    guard let prompt = raw["prompt"] as? String ?? (op == .graft ? "" : nil) else { return nil }
    return Job(
        id: id,
        op: op,
        instructions: raw["instructions"] as? String ?? "",
        prompt: prompt,
        // 600 matches what the Bedrock brief asks for. The window is 4096
        // shared between prompt and answer, so this is also the caller's
        // statement about how much room it left itself.
        maxTokens: raw["max_tokens"] as? Int ?? 600,
        schema: raw["schema"] as? [String: Any],
        history: raw["history"] as? [[String]] ?? []
    )
}

/// True when the failure was "that did not fit", rather than a real error.
///
/// Matched on the error's own text, because the typed case moved. macOS 26
/// throws `LanguageModelSession.GenerationError.exceededContextWindowSize`;
/// macOS 27 throws `LanguageModelError.contextSizeExceeded`, a type that does
/// not exist in the 26 SDK at all — so a build that pattern-matched it would
/// not compile for anyone whose Command Line Tools are a version behind. The
/// typed check below covers the case both SDKs agree on and the text covers the
/// rest.
///
/// The two texts really are that different, which is the whole reason this
/// needs more than one clause:
///
///   no tools registered   "Content contains 4120 tokens, which exceeds the
///                          maximum allowed context size of 4096."
///   tools registered      "Provided 11,085 tokens, but the maximum allowed
///                          is 4,096."
///
/// The second says neither "context" nor "exceeds" nor "window". Matching only
/// the first — which is what this did until it was measured — reports a real
/// overflow as an ordinary failure, and the caller that would have split its
/// input and retried instead gives up.
///
/// A false negative costs a failed brief; a false positive costs one wasted
/// retry at half the input.
private func looksLikeOverflow(_ error: Error) -> Bool {
    #if canImport(FoundationModels)
        if #available(macOS 26.0, *) {
            if let generation = error as? LanguageModelSession.GenerationError,
                case .exceededContextWindowSize = generation
            {
                return true
            }
        }
    #endif
    let text = String(describing: error).lowercased()
    if text.contains("context") && (text.contains("exceed") || text.contains("window")) {
        return true
    }
    // "Provided N tokens, but the maximum allowed is M." Both halves are
    // required: "maximum allowed" alone appears in unrelated framework errors.
    return text.contains("maximum allowed") && text.contains("token")
}

#if canImport(FoundationModels)

    // MARK: - Shapes

    /// Turn the caller's description of a shape into one the model is held to.
    ///
    /// The vocabulary is deliberately small — a flat object of named fields,
    /// each a string, an integer, a boolean, a list of strings, or a closed set
    /// of choices. Nothing here needs nesting, and every field a caller cannot
    /// express is a field it cannot get wrong.
    ///
    /// `desc` is not decoration. Dropping the descriptions (which is what
    /// `include_schema: false` does, and it is free in tokens) leaves the JSON
    /// perfectly valid and the answers wrong: measured 7/10 correct with them
    /// and 1/10 without, because the shape constrains the syntax while the
    /// descriptions are the only thing carrying what the fields *mean*.
    @available(macOS 26.0, *)
    private func shape(_ spec: [String: Any]) throws -> GenerationSchema {
        var properties: [DynamicGenerationSchema.Property] = []

        for field in spec["fields"] as? [[String: Any]] ?? [] {
            let name = field["name"] as? String ?? "field"
            let inner: DynamicGenerationSchema

            if let choices = field["anyOf"] as? [String] {
                inner = DynamicGenerationSchema(name: "\(name)_choice", anyOf: choices)
            } else {
                switch field["type"] as? String ?? "string" {
                case "int": inner = DynamicGenerationSchema(type: Int.self)
                case "bool": inner = DynamicGenerationSchema(type: Bool.self)
                case "[string]":
                    inner = DynamicGenerationSchema(
                        arrayOf: DynamicGenerationSchema(type: String.self))
                default: inner = DynamicGenerationSchema(type: String.self)
                }
            }

            properties.append(
                .init(
                    name: name,
                    description: field["desc"] as? String,
                    schema: inner,
                    isOptional: field["optional"] as? Bool ?? false))
        }

        let root = DynamicGenerationSchema(
            name: spec["name"] as? String ?? "Answer", properties: properties)
        return try GenerationSchema(root: root, dependencies: [])
    }

    // MARK: - Memory

    /// A transcript the caller wrote, rather than one the model accumulated.
    ///
    /// The entries are indistinguishable from real ones as far as the model is
    /// concerned, which is the point: it treats them as its own memory and will
    /// rewrite, shorten or elaborate on an answer it never actually gave in
    /// this process.
    @available(macOS 26.0, *)
    private func memory(instructions: String, history: [[String]]) -> Transcript {
        var entries: [Transcript.Entry] = []

        if !instructions.isEmpty {
            entries.append(
                .instructions(
                    Transcript.Instructions(
                        segments: [.text(Transcript.TextSegment(content: instructions))],
                        toolDefinitions: [])))
        }

        // Anything that is not a question-and-answer pair is skipped rather
        // than half-added: a prompt with no response would leave the model
        // looking at an unanswered question and trying to answer it again.
        for turn in history where turn.count == 2 {
            entries.append(
                .prompt(
                    Transcript.Prompt(
                        segments: [.text(Transcript.TextSegment(content: turn[0]))],
                        options: GenerationOptions(),
                        responseFormat: nil)))
            entries.append(
                .response(
                    Transcript.Response(
                        assetIDs: [],
                        segments: [.text(Transcript.TextSegment(content: turn[1]))])))
        }

        return Transcript(entries: entries)
    }

    // MARK: - Answering

    /// The kept session, for callers that asked for one. Nil until grafted.
    @available(macOS 26.0, *)
    private final class Kept {
        static var session: LanguageModelSession?
    }

    @available(macOS 26.0, *)
    private func run(_ job: Job) async {
        if job.op == .graft {
            Kept.session = LanguageModelSession(
                transcript: memory(instructions: job.instructions, history: job.history))
            emit(["id": job.id, "ok": true, "entries": Kept.session?.transcript.count ?? 0])
            return
        }

        let session: LanguageModelSession
        switch job.op {
        case .ask:
            // A conversation that was never grafted still deserves to start.
            if Kept.session == nil {
                Kept.session = LanguageModelSession(
                    transcript: memory(instructions: job.instructions, history: []))
            }
            session = Kept.session!
        case .once, .graft:
            // A fresh session per job, deliberately. `LanguageModelSession` keeps
            // its transcript, and every previous chunk of a long meeting would
            // then sit in the same 4096 tokens as the current one — a summary
            // that works on a ten-minute call and fails on a forty-minute one,
            // in a way that looks like the model getting worse rather than the
            // window filling up.
            session = job.instructions.isEmpty
                ? LanguageModelSession()
                : LanguageModelSession(instructions: job.instructions)
        }

        let options = GenerationOptions(
            temperature: 0.2, maximumResponseTokens: job.maxTokens)

        do {
            if let spec = job.schema {
                let reply = try await session.respond(
                    to: job.prompt,
                    schema: try shape(spec),
                    includeSchemaInPrompt: true,
                    options: options)
                emit(["id": job.id, "ok": true, "json": reply.content.jsonString])
            } else {
                let reply = try await session.respond(to: job.prompt, options: options)
                emit(["id": job.id, "ok": true, "text": reply.content])
            }
        } catch {
            emit([
                "id": job.id,
                "ok": false,
                "error": String(describing: error),
                "overflow": looksLikeOverflow(error),
            ])
        }
    }

    @available(macOS 26.0, *)
    private func serve() async -> Never {
        let answer = verdict()
        guard answer == Verdict.ok else {
            note("the on-device model is unavailable: \(answer)")
            exit(exitUnavailable)
        }

        // One job at a time, in order. There is one model on the machine and the
        // caller's work is a chain anyway — every chunk summarised before the
        // pass that reduces them — so a queue here would buy nothing but a way
        // to run out of memory on a long meeting.
        //
        // It is also the only safe shape. Two overlapping `respond` calls on one
        // session do not queue and do not throw: the process dies on a trap,
        // uncatchably, measured. Serial is not a simplification here.
        while let line = readLine(strippingNewline: true) {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed.isEmpty { continue }
            if trimmed == "quit" { exit(0) }
            guard let job = parse(trimmed) else {
                note("ignoring a line that is not a job: \(trimmed.prefix(80))")
                continue
            }
            await run(job)
        }
        // stdin closed: the app is done with us, or gone.
        exit(0)
    }
#endif

// MARK: - Entry

if CommandLine.arguments.contains("--check") {
    check()
}

#if canImport(FoundationModels)
    if #available(macOS 26.0, *) {
        await serve()
    } else {
        note("this Mac is older than macOS 26; there is no on-device model to use")
        exit(exitTooOld)
    }
#else
    note("this build has no FoundationModels SDK")
    exit(exitFailed)
#endif
