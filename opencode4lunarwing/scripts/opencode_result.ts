interface PromptPartLike {
  type: string
  text?: string
}

export function extractAssistantText(parts: PromptPartLike[] | undefined): string {
  if (!parts) return ""

  return parts
    .filter((part) => part.type === "text" && typeof part.text === "string")
    .map((part) => part.text?.trim() ?? "")
    .filter(Boolean)
    .join("\n")
}

export function opencodeErrorMessage(error: unknown): string {
  if (error instanceof Error) return error.message
  if (typeof error === "string") return error
  if (!error || typeof error !== "object") return String(error)

  const value = error as {
    name?: unknown
    message?: unknown
    data?: { message?: unknown }
  }
  if (typeof value.data?.message === "string") return value.data.message
  if (typeof value.message === "string") return value.message
  if (typeof value.name === "string") return value.name
  return "Unknown opencode error"
}
