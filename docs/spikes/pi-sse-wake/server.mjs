// Spike: SSE on a Unix socket. POST /send pushes one event to all streams.
import http from "node:http"; import fs from "node:fs";
const sock = process.argv[2]; try { fs.unlinkSync(sock); } catch {}
const clients = new Set();
http.createServer((req, res) => {
  if (req.url === "/events") {
    res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
    res.write(": hello\n\n"); clients.add(res); console.log("client connected", clients.size);
    req.on("close", () => clients.delete(res));
  } else if (req.url === "/send" && req.method === "POST") {
    let body = ""; req.on("data", (c) => (body += c)); req.on("end", () => {
      for (const c of clients) c.write(`data: ${JSON.stringify({ text: body })}\n\n`);
      console.log("sent to", clients.size, body); res.end("ok");
    });
  } else { res.writeHead(404); res.end(); }
}).listen(sock, () => console.log("listening", sock));
