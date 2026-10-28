// Borant extension for Pi and omp. Connects the session to the local Borant
// host daemon: registers the session, reports turn events, wakes the agent on
// incoming messages, and adds the Borant tools.
//
// Only node built-ins and `typebox` resolve at runtime. Pi and omp both map
// the `typebox` import to their own bundled copy. Pi types are type-only
// imports, so the same file loads in omp (`@oh-my-pi/pi-coding-agent`).
import fs from "node:fs";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import { type Static, type TSchema, Type } from "typebox";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";

// ---- Wire types (crates/borant-core/src/proto.rs, model.rs) ----

export type Target = { type: "agent"; id: string } | { type: "channel"; id: string };
export type Sender = { type: "agent"; id: string } | { type: "human" };

export interface Envelope {
  id: string;
  from: Sender;
  to: Target;
  kind: "direct" | "broadcast" | "event";
  text: string;
  urgent?: boolean;
}

export type LocalEvent =
  | { event: "turn_start" }
  | { event: "user_prompt"; text: string }
  | { event: "tool_call"; name: string; input: string }
  | { event: "tool_result"; name: string; output: string; is_error: boolean }
  | { event: "assistant_message"; text: string }
  | { event: "turn_end" }
  | { event: "end"; reason: string };

export interface EventReply {
  messages: Envelope[];
  fenced: string | null;
}

interface LocalSession {
  session: string;
  agent: string | null;
  host: string;
  state: "attached" | "detached";
  movable: boolean;
  channels: string[];
  fenced?: string | null;
  /** The agent is pinned, so a pause keeps its tools. */
  pinned?: boolean;
}

type Reply<T = unknown> = { ok: T } | { error: { code: string; message: string } };

// ---- Pure helpers ----

const KIND_TEXT: Record<Envelope["kind"], string> = {
  direct: "direct message",
  broadcast: "broadcast",
  event: "event",
};

/** The text a harness shows for one message. Matches `format_delivery` in Rust. */
export function formatDelivery(env: Envelope): string {
  const from = env.from.type === "human" ? "human" : env.from.id;
  const to = env.to.type === "channel" ? `#${env.to.id}` : env.to.id;
  return `[borant] ${KIND_TEXT[env.kind]} from ${from} to ${to} (id ${env.id}):\n${env.text}`;
}

/** `#name` is a channel. Any other text is an agent ID. */
export function parseTarget(s: string): Target {
  return s.startsWith("#") ? { type: "channel", id: s.slice(1) } : { type: "agent", id: s };
}

/** Splits an SSE byte stream into the `data` payloads of complete frames. */
export class SseParser {
  private buf = "";

  push(chunk: string): string[] {
    this.buf += chunk.replace(/\r\n?/g, "\n");
    const out: string[] = [];
    let i: number;
    while ((i = this.buf.indexOf("\n\n")) >= 0) {
      const frame = this.buf.slice(0, i);
      this.buf = this.buf.slice(i + 2);
      const data = frame
        .split("\n")
        .filter((l) => l.startsWith("data:"))
        .map((l) => l.slice(l.startsWith("data: ") ? 6 : 5));
      if (data.length > 0) out.push(data.join("\n"));
    }
    return out;
  }
}

type ContentPart = { type: string; text?: string };

export function contentText(content: readonly ContentPart[] | undefined): string {
  return (content ?? [])
    .map((p) => (p.type === "text" ? (p.text ?? "") : `[${p.type}]`))
    .join("\n");
}

/** The text of the last assistant message that has text, or undefined. */
export function lastAssistantText(messages: readonly unknown[]): string | undefined {
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i] as { role?: string; content?: unknown };
    if (m?.role !== "assistant" || !Array.isArray(m.content)) continue;
    const text = (m.content as ContentPart[])
      .filter((p) => p.type === "text" && p.text)
      .map((p) => p.text)
      .join("\n");
    if (text) return text;
  }
  return undefined;
}

export function promptEvents(text: string): LocalEvent[] {
  return [{ event: "turn_start" }, { event: "user_prompt", text }];
}

export function toolEvents(
  name: string,
  input: unknown,
  content: readonly ContentPart[] | undefined,
  isError: boolean,
): LocalEvent[] {
  return [
    { event: "tool_call", name, input: JSON.stringify(input ?? {}) },
    { event: "tool_result", name, output: contentText(content), is_error: isError },
  ];
}

export function endEvents(messages: readonly unknown[]): LocalEvent[] {
  const text = lastAssistantText(messages);
  const out: LocalEvent[] = text ? [{ event: "assistant_message", text }] : [];
  out.push({ event: "turn_end" });
  return out;
}

export function socketPath(env: NodeJS.ProcessEnv = process.env): string {
  if (env.BORANT_SOCKET) return env.BORANT_SOCKET;
  return path.join(env.BORANT_HOME || path.join(os.homedir(), ".borant"), "host.sock");
}

// ---- HTTP on the Unix socket ----

function request<T>(
  socket: string,
  method: string,
  urlPath: string,
  body: unknown,
  timeoutMs: number,
): Promise<Reply<T>> {
  return new Promise((resolve, reject) => {
    const payload = body === undefined ? undefined : JSON.stringify(body);
    const req = http.request(
      {
        socketPath: socket,
        path: urlPath,
        method,
        headers: payload ? { "content-type": "application/json" } : {},
      },
      (res) => {
        let text = "";
        res.setEncoding("utf8");
        res.on("data", (c) => (text += c));
        res.on("end", () => {
          try {
            const r = JSON.parse(text) as Reply<T>;
            if (r && ("ok" in r || "error" in r)) resolve(r);
            else reject(new Error(`bad reply: ${text.slice(0, 200)}`));
          } catch {
            reject(new Error(`bad reply (HTTP ${res.statusCode}): ${text.slice(0, 200)}`));
          }
        });
        res.on("error", reject);
      },
    );
    req.setTimeout(timeoutMs, () => req.destroy(new Error(`timeout after ${timeoutMs} ms`)));
    req.on("error", reject);
    req.end(payload);
  });
}

const RETRY_MS = 5000;
const EVENT_TIMEOUT_MS = 2000;
// The daemon waits up to 3 s for the hub on registration.
const REGISTER_TIMEOUT_MS = 10000;
const CALL_TIMEOUT_MS = 30000;
const SHUTDOWN_WAIT_MS = 1500;
// Longest wait for the harness's own first prompt (omp connects MCP servers first).
const STARTUP_GRACE_MS = 10000;

let warned = false;
function warnOnce(msg: string): void {
  if (warned) return;
  warned = true;
  console.error(`borant: ${msg}`);
}

// ---- Tool schemas ----

function StringEnum<const T extends readonly string[]>(values: T, description: string) {
  return Type.Unsafe<T[number]>({ type: "string", enum: [...values], description });
}

const JsonObject = (description: string) =>
  Type.Record(Type.String(), Type.Unknown(), { description });
const Channel = Type.String({ description: "Channel name, without #" });
const TaskId = Type.String({ description: "Task ID" });
const TaskIds = (description: string) => Type.Array(Type.String(), { description });
const TaskStatus = StringEnum(
  ["open", "in_progress", "needs_human", "stuck", "closed"] as const,
  "New status",
);
const CloseReason = StringEnum(
  ["done", "wontfix", "duplicate", "superseded"] as const,
  "Why the task is closed",
);
const ChannelKind = StringEnum(
  ["project", "saga", "epic"] as const,
  "Kind of channel: project (default), saga, or epic",
);
const TaskKind = StringEnum(["task", "epic"] as const, "Kind of task: task (default) or epic");
const HarnessName = StringEnum(
  ["claude-code", "pi", "omp", "codex"] as const,
  "Harness for the new agent",
);

// ---- Extension ----

interface Session {
  id: string;
  socket: string;
  /** omp wakes an idle agent only without `deliverAs`; Pi needs it when busy. */
  omp: boolean;
  ctx: ExtensionContext;
  fenced: string | null;
  /** The agent is pinned: a pause keeps every tool. */
  pinned: boolean;
  closed: boolean;
  /** False until the harness starts its first run or the grace time passes. */
  started: boolean;
  held: string[];
  chain: Promise<void>;
  graceTimer?: ReturnType<typeof setTimeout>;
  stream?: http.ClientRequest;
  retryTimer?: ReturnType<typeof setTimeout>;
  attachTimer?: ReturnType<typeof setInterval>;
}

export default function borant(pi: ExtensionAPI) {
  let sess: Session | undefined;
  let toolsRegistered = false;

  function deliver(s: Session, text: string): void {
    if (s.closed) return;
    // A harness started with a prompt (`omp "task"`, `pi -p`) sends it after
    // session_start. A wake before that makes its own prompt fail as busy.
    if (!s.started && s.ctx.isIdle()) {
      s.held.push(text);
      return;
    }
    try {
      // Pi: "steer" starts a run when idle and steers when busy. Without it,
      // a second message while the first run starts throws and is lost.
      // omp: without `deliverAs`, it starts a run when idle and queues a
      // steer when busy; an explicit "steer" never wakes an idle omp.
      if (s.omp) pi.sendUserMessage(text);
      else pi.sendUserMessage(text, { deliverAs: "steer" });
    } catch (e) {
      console.error("borant: could not deliver a message:", e);
    }
  }

  function release(s: Session): void {
    if (s.started) return;
    s.started = true;
    clearTimeout(s.graceTimer);
    for (const text of s.held.splice(0)) deliver(s, text);
  }

  /** Posts events in order. Never throws and never blocks the harness. */
  function post(s: Session, events: LocalEvent[]): Promise<void> {
    for (const ev of events) {
      s.chain = s.chain
        .then(async () => {
          const r = await request<EventReply>(
            s.socket,
            "POST",
            `/v1/sessions/${s.id}/events`,
            ev,
            EVENT_TIMEOUT_MS,
          );
          if (!("ok" in r)) return;
          s.fenced = r.ok.fenced ?? null;
          for (const m of r.ok.messages ?? []) deliver(s, formatDelivery(m));
        })
        .catch(() => {});
    }
    return s.chain;
  }

  function openStream(s: Session): void {
    if (s.closed) return;
    let done = false;
    const retry = () => {
      if (done) return;
      done = true;
      if (s.closed) return;
      s.retryTimer = setTimeout(() => openStream(s), RETRY_MS);
      s.retryTimer.unref?.();
    };
    const parser = new SseParser();
    const req = http.request(
      {
        socketPath: s.socket,
        path: `/v1/sessions/${s.id}/stream`,
        method: "GET",
        headers: { accept: "text/event-stream" },
      },
      (res) => {
        if (res.statusCode !== 200) {
          res.resume();
          retry();
          return;
        }
        res.setEncoding("utf8");
        res.on("data", (chunk: string) => {
          for (const data of parser.push(chunk)) {
            try {
              deliver(s, formatDelivery(JSON.parse(data) as Envelope));
            } catch {
              // Skip a bad frame.
            }
          }
        });
        res.on("end", retry);
        res.on("error", retry);
      },
    );
    req.on("socket", (sock) => sock.unref?.());
    req.on("error", retry);
    req.on("close", retry);
    req.end();
    s.stream = req;
  }

  /** The daemon retries the hub in the background. Poll until attached. */
  function watchAttach(s: Session): void {
    s.attachTimer = setInterval(async () => {
      try {
        const r = await request<LocalSession>(s.socket, "GET", `/v1/sessions/${s.id}`, undefined, EVENT_TIMEOUT_MS);
        if ("ok" in r && r.ok.state === "attached") clearInterval(s.attachTimer);
      } catch {
        // Try again on the next tick.
      }
    }, RETRY_MS);
    s.attachTimer.unref?.();
  }

  function registerTools(): void {
    if (toolsRegistered) return;
    toolsRegistered = true;

    function tool<T extends TSchema>(
      name: string,
      description: string,
      parameters: T,
      toCall: (p: Static<T>) => Record<string, unknown> = (p) => ({ ...(p as object) }),
    ): void {
      pi.registerTool({
        name,
        label: `Borant ${name}`,
        description,
        parameters,
        async execute(_id, params) {
          const s = sess;
          if (!s) throw new Error("No Borant session. The host daemon may be down.");
          const r = await request(
            s.socket,
            "POST",
            "/v1/call",
            { session: s.id, call: { tool: name, ...toCall(params) } },
            CALL_TIMEOUT_MS,
          );
          if ("error" in r) throw new Error(`${r.error.code}: ${r.error.message}`);
          return {
            content: [{ type: "text", text: JSON.stringify(r.ok, null, 2) }],
            details: undefined,
          };
        },
      });
    }

    const none = Type.Object({});
    const name = Type.String({ description: "Channel name, without #" });

    tool("whoami", "Show your Borant agent ID, session, host, and channels.", none);
    tool(
      "channel_create",
      "Create a channel. You become its owner.",
      Type.Object({
        name,
        kind: Type.Optional(ChannelKind),
        title: Type.Optional(Type.String({ description: "Display name; a saga or epic channel takes its name when omitted" })),
        issue: Type.Optional(Type.String({ description: "Issue link of a saga" })),
        issue_target: Type.Optional(Type.String({ description: "Issue tracker team for this item and the items under it, for example ENG. An empty string clears it" })),
        schema: Type.Optional(JsonObject("JSON Schema for task properties. Default schema if omitted.")),
        project: Type.Optional(Type.String({ description: "Project name. A spawn in the channel clones it" })),
        branch: Type.Optional(Type.String({ description: "Base branch for spawns. Default: the project's default branch" })),
      }),
    );
    tool("channel_join", "Join a channel.", Type.Object({ name }));
    tool("channel_leave", "Leave a channel.", Type.Object({ name }));
    tool("channel_list", "List channels.", none);
    tool(
      "channel_transfer",
      "Give ownership of a channel you own to another agent.",
      Type.Object({ name, to: Type.String({ description: "Agent ID of the new owner" }) }),
    );
    tool(
      "channel_close",
      "Close a channel. Only the owner or a person closes it, and only when every task of the channel is closed.",
      Type.Object({ name }),
    );
    tool("members", "List the members of a channel.", Type.Object({ name }));
    tool(
      "send",
      "Send a message to an agent or a channel.",
      Type.Object({
        to: Type.String({ description: "Agent ID, or #name for a channel" }),
        text: Type.String({ description: "Message text" }),
        urgent: Type.Optional(Type.Boolean({ description: "Interrupt the receiver now" })),
      }),
      (p) => ({ to: parseTarget(p.to), text: p.text, urgent: p.urgent ?? false }),
    );
    tool(
      "task_create",
      "Create a task in a channel.",
      Type.Object({
        channel: Channel,
        kind: Type.Optional(TaskKind),
        issue: Type.Optional(Type.String({ description: "Issue link of the task" })),
        issue_target: Type.Optional(Type.String({ description: "Issue tracker team for this item and the items under it, for example ENG. An empty string clears it" })),
        title: Type.String({ description: "Task title" }),
        body: Type.Optional(Type.String({ description: "Task details" })),
        blocked_by: Type.Optional(TaskIds("IDs of tasks that must close first")),
        parent: Type.Optional(Type.String({ description: "Parent task ID" })),
        properties: Type.Optional(JsonObject("Task properties, checked against the channel schema")),
      }),
      ({ channel, ...task }) => ({ channel, task }),
    );
    tool("task_get", "Get one task.", Type.Object({ channel: Channel, id: TaskId }));
    tool(
      "task_list",
      "List the tasks of a channel.",
      Type.Object({
        channel: Channel,
        ready_only: Type.Optional(Type.Boolean({ description: "Only open, unblocked, unclaimed tasks" })),
      }),
    );
    tool(
      "task_update",
      "Change fields of a task. Properties merge key by key; a null value removes a key.",
      Type.Object({
        channel: Channel,
        id: TaskId,
        title: Type.Optional(Type.String({ description: "New title" })),
        body: Type.Optional(Type.String({ description: "New body" })),
        issue: Type.Optional(Type.String({ description: "Issue link of the task" })),
        issue_target: Type.Optional(Type.String({ description: "Issue tracker team for this item and the items under it, for example ENG. An empty string clears it" })),
        status: Type.Optional(TaskStatus),
        attention_msg: Type.Optional(Type.String({ description: "What a human or the owner must know or do" })),
        blocked_by: Type.Optional(TaskIds("New list of blocking task IDs")),
        properties: Type.Optional(JsonObject("Properties to merge")),
      }),
      ({ channel, id, ...patch }) => ({ channel, id, patch }),
    );
    tool("task_claim", "Claim a task so that you work on it.", Type.Object({ channel: Channel, id: TaskId }));
    tool(
      "task_close",
      "Close a task.",
      Type.Object({
        channel: Channel,
        id: TaskId,
        reason: CloseReason,
        message: Type.String({ description: "What was done, for the channel owner" }),
      }),
    );
    tool("schema_get", "Get the task property schema of a channel.", Type.Object({ channel: Channel }));
    tool(
      "schema_extend",
      "Add properties to the task schema of a channel you own. Keys can only be added.",
      Type.Object({ channel: Channel, properties: JsonObject("JSON Schema of each new property, by key") }),
    );
    tool(
      "spawn",
      "Start a new agent that joins a channel and works on a prompt.",
      Type.Object({
        channel: Channel,
        prompt: Type.String({ description: "First prompt for the new agent" }),
        harness: Type.Optional(HarnessName),
        host: Type.Optional(Type.String({ description: "Host to run on" })),
        name: Type.Optional(Type.String({ description: "Agent ID for the new agent" })),
        model: Type.Optional(Type.String({ description: "Model for the harness, for example opus" })),
      }),
    );
    tool("project_list", "List projects: git repositories that spawns clone.", none);
    tool("project_get", "Get one project.", Type.Object({ name: Type.String({ description: "Project name" }) }));
    tool(
      "board",
      "Read the board: the sagas, epics, and waiting questions of your collection, computed from the records. Nothing is stored.",
      Type.Object({ collection: Type.Optional(Type.String({ description: "One collection of hub.toml; omit for every collection you may see" })) }),
    );
    tool(
      "session_history",
      "Read the event history of your session, newest page first.",
      Type.Object({
        page: Type.Optional(Type.Integer({ minimum: 0, description: "Page number, from 0" })),
        page_size: Type.Optional(Type.Integer({ minimum: 1, description: "Events per page, default 100" })),
      }),
    );
  }

  let starting: ReturnType<typeof setTimeout> | undefined;
  let stopped = false;

  pi.on("session_start", async (_event, ctx) => {
    stopped = false;
    await start(ctx);
  });

  /** Registers with the daemon, and retries every 5 s while it fails. */
  async function start(ctx: ExtensionContext): Promise<void> {
    if (stopped || sess) return;
    // omp re-binds extensions in subagent sessions; connect only the main one.
    const agent = (ctx as { agent?: { kind?: string } }).agent;
    if (agent && agent.kind !== "main") return;
    const socket = socketPath();
    // No socket means no daemon here: stay silent (AGENTS.md).
    if (!fs.existsSync(socket)) return;
    const retry = () => {
      starting = setTimeout(() => void start(ctx), 5000);
      starting.unref?.();
    };
    let r: Reply<LocalSession>;
    try {
      r = await request<LocalSession>(
        socket,
        "POST",
        "/v1/sessions",
        {
          harness: agent ? "omp" : "pi",
          harness_session_id: ctx.sessionManager.getSessionId(),
          pid: process.pid,
          cwd: ctx.cwd,
          agent: process.env.BORANT_AGENT || undefined,
          channel: process.env.BORANT_CHANNEL || undefined,
          // A chief or a worker that a person starts reports its role.
          role: process.env.BORANT_ROLE || undefined,
          collection: process.env.BORANT_COLLECTION || undefined,
        },
        REGISTER_TIMEOUT_MS,
      );
    } catch (e) {
      warnOnce(`host daemon not reachable at ${socket}: ${(e as Error).message}`);
      retry();
      return;
    }
    if ("error" in r) {
      warnOnce(`session registration failed: ${r.error.message}`);
      retry();
      return;
    }
    const s: Session = {
      id: r.ok.session,
      socket,
      omp: Boolean(agent),
      ctx,
      fenced: null,
      pinned: r.ok.pinned ?? false,
      closed: false,
      started: false,
      held: [],
      chain: Promise.resolve(),
    };
    sess = s;
    s.graceTimer = setTimeout(() => release(s), STARTUP_GRACE_MS);
    s.graceTimer.unref?.();
    registerTools();
    openStream(s);
    if (r.ok.state === "detached") watchAttach(s);
  }

  pi.on("agent_start", () => {
    if (sess) release(sess);
  });

  pi.on("before_agent_start", (event) => {
    const prompt = (event as { prompt?: unknown }).prompt;
    if (sess && typeof prompt === "string") void post(sess, promptEvents(prompt));
  });

  pi.on("tool_call", async () => {
    const s = sess;
    if (!s) return;
    // Ask the daemon, so a pause that began while the agent was idle counts.
    try {
      const r = await request<LocalSession>(s.socket, "GET", `/v1/sessions/${s.id}`, undefined, 1000);
      if ("ok" in r) {
        s.fenced = r.ok.fenced ?? null;
        s.pinned = r.ok.pinned ?? false;
      }
    } catch {
      // Keep the last known state.
    }
    if (s.fenced && !s.pinned) return { block: true, reason: s.fenced };
  });

  pi.on("tool_result", (event) => {
    if (sess) void post(sess, toolEvents(event.toolName, event.input, event.content, event.isError));
  });

  pi.on("agent_end", (event) => {
    if (sess) void post(sess, endEvents(event.messages));
  });

  pi.on("session_shutdown", async () => {
    stopped = true;
    clearTimeout(starting);
    const s = sess;
    if (!s) return;
    sess = undefined;
    s.closed = true;
    clearTimeout(s.retryTimer);
    clearTimeout(s.graceTimer);
    clearInterval(s.attachTimer);
    s.stream?.destroy();
    let timer: ReturnType<typeof setTimeout> | undefined;
    await Promise.race([
      post(s, [{ event: "end", reason: "session shutdown" }]),
      new Promise<void>((resolve) => (timer = setTimeout(resolve, SHUTDOWN_WAIT_MS))),
    ]);
    clearTimeout(timer);
  });
}
