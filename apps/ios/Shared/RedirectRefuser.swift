import Foundation

/// Hands the redirect response itself back instead of following it: a password, or a proof that
/// names the first URL, is never re-sent to wherever a redirect points. Also the server check and
/// the local sign-in use it.
final class RedirectRefuser: NSObject, URLSessionTaskDelegate {
  func urlSession(
    _ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
    newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void
  ) {
    completionHandler(nil)
  }
}
