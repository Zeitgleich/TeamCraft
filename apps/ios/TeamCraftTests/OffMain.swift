import Foundation

/// Runs `work` off the main thread and waits for it there, as the core's engine thread does (#2184):
/// a test on the main thread that called a blocking token read directly would wait on the main
/// thread, which the app never does, and the run reports it as a priority inversion.
func offMain<T>(_ work: @escaping () -> T) async -> T {
  await Task.detached { work() }.value
}
