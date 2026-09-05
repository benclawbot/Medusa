const MAX_USER_ERROR_LENGTH = 280;
const LOCAL_PATH_PATTERN = /(?:[A-Za-z]:[\\/]|\/(?:Users|home|tmp|var|private)\/)[^\s"'`]+/g;
const SECRET_PATTERN = /\b(api[_-]?key|access[_-]?token|token|password|authorization)\s*[:=]\s*[^\s,;]+/gi;

export function toUserError(cause: unknown): string {
  const raw = cause instanceof Error ? cause.message : String(cause ?? "");
  const normalized = raw
    .replace(SECRET_PATTERN, "$1=[redacted]")
    .replace(LOCAL_PATH_PATTERN, "[local path]")
    .trim();
  if (!normalized) return "The operation failed unexpectedly.";
  if (normalized.length <= MAX_USER_ERROR_LENGTH) return normalized;
  return `${normalized.slice(0, MAX_USER_ERROR_LENGTH - 1)}…`;
}
