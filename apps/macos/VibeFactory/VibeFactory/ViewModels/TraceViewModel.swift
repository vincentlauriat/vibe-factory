import Foundation
import Observation
import VibeAPI

/// The Trace tab of a task: the runs to pick from, the calls of the picked
/// run, which are expanded, and the complete outputs loaded on demand.
///
/// Memory stays bounded: only the picked run's calls are kept, at most
/// `outputLimit` outputs of at most `outputChars` characters each, and the
/// outputs are dropped when another run is picked.
@Observable
@MainActor
final class TraceViewModel {
    /// A complete output, loaded on demand.
    enum Output: Equatable {
        case loading
        /// `clipped`: longer than `outputChars`, only the start is shown.
        case loaded(text: String, truncated: Bool, clipped: Bool)
        /// Not traced, discarded, or unreadable (404).
        case missing
        case failed(String)
    }

    /// A run of the task and its number of calls.
    struct RunChoice: Identifiable, Hashable {
        let id: String
        let calls: Int
    }

    static let outputLimit = 4
    static let outputChars = 200_000

    let taskId: String
    private(set) var runs: [RunChoice] = []
    private(set) var trace: RunTrace?
    /// The task has not been run yet (404).
    private(set) var notRun = false
    private(set) var loading = false
    /// Indices of the expanded calls in `trace.calls` (call ids repeat
    /// before 0.5).
    var expanded: Set<Int> = []
    private(set) var outputs: [Int: Output] = [:]
    var errorMessage: String?

    var selectedRun: String? { trace?.run }

    @ObservationIgnored private let client: VibeClient
    @ObservationIgnored private var loadTask: Task<Void, Never>?
    @ObservationIgnored private var outputTasks: [Int: Task<Void, Never>] = [:]
    /// Order in which outputs were loaded, oldest first.
    @ObservationIgnored private var outputOrder: [Int] = []

    init(taskId: String, client: VibeClient) {
        self.taskId = taskId
        self.client = client
    }

    /// Whether something was shown: a run, or the "not run" state.
    private var shown: Bool { trace != nil || notRun }

    /// The tab appeared: load unless something is shown already (a load cut
    /// by `stop()` is started again).
    func appear() {
        if !shown { reload() }
    }

    /// The tab disappeared or the task changed: cancel the loads.
    func stop() {
        loadTask?.cancel()
        loadTask = nil
        loading = false
        dropOutputs()
    }

    /// Every run with its calls. The picked run is kept, unless it was the
    /// last one: then the newest run is followed.
    func reload() {
        let keep = runs.last?.id == selectedRun ? nil : selectedRun
        load { client, taskId in
            let all = try await client.trace(task: taskId, all: true)
            return (all, all.first { $0.run == keep } ?? all.last)
        }
    }

    func select(run: String) {
        guard run != selectedRun else { return }
        load { client, taskId in
            (nil, try await client.trace(task: taskId, run: run).first)
        }
    }

    /// A run of this task ended: its trace is complete now.
    func runFinished() {
        if shown { reload() }
    }

    private func load(_ fetch: @escaping (VibeClient, String) async throws -> ([RunTrace]?, RunTrace?)) {
        loadTask?.cancel()
        loading = true
        loadTask = Task { [weak self, client, taskId] in
            do {
                let (all, picked) = try await fetch(client, taskId)
                self?.show(all: all, picked: picked)
            } catch is CancellationError {
                return
            } catch VibeError.notFound {
                self?.show(all: [], picked: nil)
            } catch {
                self?.errorMessage = error.localizedDescription
            }
            self?.loading = false
        }
    }

    private func show(all: [RunTrace]?, picked: RunTrace?) {
        if let all {
            runs = all.map { RunChoice(id: $0.run, calls: $0.calls.count) }
            notRun = all.isEmpty
        }
        if picked?.run != trace?.run {
            expanded = []
            dropOutputs()
        }
        trace = picked
    }

    // MARK: Outputs

    func toggle(_ index: Int) {
        if expanded.contains(index) { expanded.remove(index) } else { expanded.insert(index) }
    }

    func loadOutput(_ index: Int) {
        guard let call = trace?.calls[safe: index], call.hasOutput else { return }
        outputTasks[index]?.cancel()
        outputs[index] = .loading
        remember(index)
        outputTasks[index] = Task { [weak self, client, taskId] in
            let result: Output
            do {
                if let output = try await client.traceOutput(task: taskId, call: call.call) {
                    let clipped = output.text.count > Self.outputChars
                    result = .loaded(text: clipped ? String(output.text.prefix(Self.outputChars)) : output.text,
                                     truncated: output.truncated, clipped: clipped)
                } else {
                    result = .missing
                }
            } catch is CancellationError {
                return
            } catch {
                result = .failed(error.localizedDescription)
            }
            self?.outputs[index] = result
            self?.outputTasks[index] = nil
        }
    }

    /// Keep the last `outputLimit` outputs.
    private func remember(_ index: Int) {
        outputOrder.removeAll { $0 == index }
        outputOrder.append(index)
        while outputOrder.count > Self.outputLimit {
            let old = outputOrder.removeFirst()
            outputTasks[old]?.cancel()
            outputTasks[old] = nil
            outputs[old] = nil
        }
    }

    private func dropOutputs() {
        outputTasks.values.forEach { $0.cancel() }
        outputTasks = [:]
        outputs = [:]
        outputOrder = []
    }
}

extension Array {
    subscript(safe index: Int) -> Element? {
        indices.contains(index) ? self[index] : nil
    }
}
