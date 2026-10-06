// Pure helpers for chat hydration. No imports: the logic suite runs under
// `node --experimental-strip-types --test` without the web alias config.

/** The shape of a transcript bubble that these helpers read. */
export interface HydrationBubble {
  role: 'user' | 'agent';
  content: string;
  local?: boolean;
}

// The runtime prefixes every user turn it persists with
// `[CURRENT DATE & TIME: <date> <tz>]\n\n` (agent.rs `enrich_user_message`).
const ENRICHMENT_PREFIX_RE = /^\s*\[CURRENT DATE & TIME: [^\]]*\]\s*/;

/** A persisted user row's content with the runtime's date prefix removed. */
export function normalizeServerUserContent(content: string): string {
  return content.replace(ENRICHMENT_PREFIX_RE, '');
}

/**
 * The tail of the local transcript that a server snapshot cannot contain yet.
 *
 * The gateway commits a turn's user prompt only when the turn finishes, so a
 * snapshot fetched while a detached turn is still running predates the prompt
 * the browser already showed. The tail starts at the last user bubble the
 * browser composed itself (`local: true`) and runs to the end of the local
 * transcript, and is returned only when that prompt has more occurrences in the
 * local transcript than in the snapshot. Counting occurrences, rather than
 * testing presence, keeps a repeated prompt ("continue") from being mistaken
 * for one the snapshot already holds.
 */
export function uncommittedLocalTail<T extends HydrationBubble>(
  snapshotUserContents: readonly string[],
  local: readonly T[],
): T[] {
  let start = -1;
  for (let i = local.length - 1; i >= 0; i -= 1) {
    if (local[i]!.role === 'user' && local[i]!.local === true) {
      start = i;
      break;
    }
  }
  if (start < 0) return [];

  const prompt = normalizeServerUserContent(local[start]!.content);
  const inSnapshot = snapshotUserContents.filter(
    (content) => normalizeServerUserContent(content) === prompt,
  ).length;
  const inLocal = local.filter(
    (m) => m.role === 'user' && normalizeServerUserContent(m.content) === prompt,
  ).length;
  return inLocal > inSnapshot ? local.slice(start) : [];
}
