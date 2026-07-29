/**
 * kimi_result.ts — Helpers for extracting assistant text and error messages
 * from Kimi Code CLI stream-json output.
 */

export function extractAssistantText(messages: readonly KimiMessage[]): string {
  if (!Array.isArray(messages)) return ""
  // The canonical result is the LAST assistant message with non-empty
  // content — intermediate thinking/tool-call messages are progress, not
  // the answer (matches collectOutput in the task executor).
  for (let i = messages.length - 1; i >= 0; i--) {
    const msg = messages[i]
    if (!msg || msg.role !== "assistant") continue
    const content = typeof msg.content === "string" ? msg.content.trim() : ""
    if (content.length > 0) return content
  }
  return ""
}

export function kimiErrorMessage(error: unknown): string {
  if (error === undefined || error === null) return "Unknown kimi error"
  if (error instanceof Error) return error.message || "Unknown kimi error"
  if (typeof error === "string") return error
  if (typeof error === "object") {
    const e = error as Record<string, unknown>
    const message = e.message
    if (typeof message === "string" && message.length > 0) return message
    const stderr = e.stderr
    if (typeof stderr === "string" && stderr.length > 0) return stderr
  }
  return "Unknown kimi error"
}

export interface KimiMessage {
  role: string
  content?: string
  tool_calls?: Array<{
    id: string
    type: string
    function: { name: string; arguments: string }
  }>
  tool_call_id?: string
}
