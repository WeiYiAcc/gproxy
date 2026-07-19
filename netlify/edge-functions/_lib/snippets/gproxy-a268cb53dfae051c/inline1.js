
export async function gproxyResponsesWebSocketRoundTrip(url, headerEntries, frame) {
  const headers = new Headers();
  for (const pair of headerEntries) {
    headers.append(pair[0], pair[1]);
  }
  headers.set("Upgrade", "websocket");

  const response = await fetch(url, { method: "GET", headers });
  const socket = response.webSocket;
  if (!socket) {
    throw new Error(`websocket upgrade failed with status ${response.status}`);
  }

  if (typeof socket.accept === "function") {
    socket.accept();
  }

  const decoder = new TextDecoder();
  const messages = [];
  const terminal = new Set(["response.completed", "response.done", "response.failed", "error"]);

  return await new Promise((resolve, reject) => {
    let settled = false;
    const finish = () => {
      if (settled) return;
      settled = true;
      try { socket.close(); } catch (_) {}
      resolve(messages);
    };
    const fail = (message) => {
      if (settled) return;
      settled = true;
      try { socket.close(); } catch (_) {}
      reject(new Error(message));
    };

    socket.addEventListener("message", (event) => {
      const text = typeof event.data === "string" ? event.data : decoder.decode(event.data);
      messages.push(text);
      let kind = null;
      try { kind = JSON.parse(text)?.type ?? null; } catch (_) {}
      if (terminal.has(kind)) {
        finish();
      }
    });
    socket.addEventListener("close", () => {
      if (settled) return;
      fail("websocket closed before terminal response");
    });
    socket.addEventListener("error", () => fail("websocket error"));

    try {
      socket.send(frame);
    } catch (error) {
      fail(error?.message ?? String(error));
    }
  });
}
