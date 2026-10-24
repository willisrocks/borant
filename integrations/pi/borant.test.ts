import { describe, expect, test } from "bun:test";
import { endEvents, formatDelivery, parseTarget, SseParser, toolEvents } from "./borant.ts";

describe("formatDelivery", () => {
  test("broadcast from an agent to a channel", () => {
    const text = formatDelivery({
      id: "01J0",
      from: { type: "agent", id: "boss" },
      to: { type: "channel", id: "api" },
      kind: "broadcast",
      text: "ship it",
    });
    expect(text).toBe("[borant] broadcast from boss to #api (id 01J0):\nship it");
  });

  test("direct message from a human to an agent", () => {
    const text = formatDelivery({
      id: "01J1",
      from: { type: "human" },
      to: { type: "agent", id: "w1" },
      kind: "direct",
      text: "hi",
    });
    expect(text).toBe("[borant] direct message from human to w1 (id 01J1):\nhi");
  });
});

describe("parseTarget", () => {
  test("#name is a channel", () => {
    expect(parseTarget("#api")).toEqual({ type: "channel", id: "api" });
  });
  test("anything else is an agent", () => {
    expect(parseTarget("w1")).toEqual({ type: "agent", id: "w1" });
  });
});

describe("SseParser", () => {
  test("joins frames split across chunks", () => {
    const p = new SseParser();
    expect(p.push("event: message\nda")).toEqual([]);
    expect(p.push('ta: {"a":1}\n')).toEqual([]);
    expect(p.push('\nevent: message\ndata: {"b":2}\n\n:keep-alive\n\n')).toEqual([
      '{"a":1}',
      '{"b":2}',
    ]);
  });

  test("handles CRLF and multi-line data", () => {
    const p = new SseParser();
    expect(p.push("data: one\r\ndata: two\r\n\r\n")).toEqual(["one\ntwo"]);
  });
});

describe("event mapping", () => {
  test("tool result maps to tool_call and tool_result", () => {
    expect(toolEvents("bash", { command: "ls" }, [{ type: "text", text: "a.txt" }], false)).toEqual([
      { event: "tool_call", name: "bash", input: '{"command":"ls"}' },
      { event: "tool_result", name: "bash", output: "a.txt", is_error: false },
    ]);
  });

  test("agent end sends the last assistant text, then turn_end", () => {
    const messages = [
      { role: "assistant", content: [{ type: "text", text: "first" }] },
      { role: "toolResult", content: [{ type: "text", text: "x" }] },
      { role: "assistant", content: [{ type: "text", text: "done" }] },
    ];
    expect(endEvents(messages)).toEqual([
      { event: "assistant_message", text: "done" },
      { event: "turn_end" },
    ]);
    expect(endEvents([])).toEqual([{ event: "turn_end" }]);
  });
});
