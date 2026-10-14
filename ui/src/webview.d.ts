// Окно программы (src/WebWindow.cs) говорит со страницей сообщениями WebView2.
interface WebViewBridge {
  postMessage(message: unknown): void;
  addEventListener(type: "message", listener: (event: MessageEvent) => void): void;
}

interface Window {
  chrome: { webview: WebViewBridge };
}
