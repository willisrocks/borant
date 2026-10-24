// Spike: hold an SSE stream on a Unix socket and wake the agent on each event.
import http from "node:http";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

export default function (pi: ExtensionAPI) {
  let req: http.ClientRequest | undefined;
  pi.on("session_start", async (_event, ctx) => {
    req = http.get({ socketPath: process.env.SPIKE_SOCK, path: "/events" }, (res) => {
      let buf = "";
      res.on("data", (chunk) => {
        buf += chunk;
        let i;
        while ((i = buf.indexOf("\n\n")) >= 0) {
          const frame = buf.slice(0, i); buf = buf.slice(i + 2);
          const data = frame.split("\n").find((l) => l.startsWith("data: "));
          if (!data) continue;
          const { text } = JSON.parse(data.slice(6));
          if (ctx.isIdle()) pi.sendUserMessage(text);
          else pi.sendUserMessage(text, { deliverAs: "steer" });
        }
      });
    });
    req.on("error", (e) => console.error("spike sse error", e));
  });
  pi.on("session_shutdown", async () => { req?.destroy(); req = undefined; });
}
