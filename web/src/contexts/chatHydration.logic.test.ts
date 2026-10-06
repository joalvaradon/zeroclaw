import assert from 'node:assert/strict';
import test from 'node:test';
import {
  normalizeServerUserContent,
  uncommittedLocalTail,
  type HydrationBubble,
} from './chatHydration.logic.ts';

const stamp = (prompt: string) => `[CURRENT DATE & TIME: 2026-10-01 10:00:00 UTC]\n\n${prompt}`;
const user = (content: string, local = false): HydrationBubble => ({ role: 'user', content, local });
const agent = (content: string): HydrationBubble => ({ role: 'agent', content });

test('strips the runtime date prefix the gateway persists on user rows', () => {
  assert.equal(normalizeServerUserContent(stamp('long task')), 'long task');
  assert.equal(normalizeServerUserContent('long task'), 'long task');
  assert.equal(
    normalizeServerUserContent('Explain [CURRENT DATE & TIME: x] literally'),
    'Explain [CURRENT DATE & TIME: x] literally',
  );
});

test('an empty snapshot keeps the in-flight prompt and what followed it', () => {
  const local = [user('long task', true), agent('partial tool bubble')];
  assert.deepEqual(uncommittedLocalTail([], local), local);
});

test('a snapshot that already holds the prompt yields no tail', () => {
  const local = [user('long task', true), agent('done')];
  assert.deepEqual(uncommittedLocalTail([stamp('long task')], local), []);
});

test('only the last prompt and its trailing bubbles are returned', () => {
  const local = [user('one', true), agent('a1'), user('two', true), agent('tool')];
  assert.deepEqual(
    uncommittedLocalTail([stamp('one')], local),
    [user('two', true), agent('tool')],
  );
});

test('a repeated prompt is not mistaken for the committed one', () => {
  const local = [user('continue', true), agent('r1'), user('continue', true)];
  assert.deepEqual(uncommittedLocalTail([stamp('continue')], local), [user('continue', true)]);
  assert.deepEqual(
    uncommittedLocalTail([stamp('continue'), stamp('continue')], local),
    [],
  );
});

test('server-rebuilt bubbles from an earlier reload count toward the local occurrences', () => {
  // After a prior reload the older "continue" is a server-rebuilt (non-local,
  // still date-prefixed) bubble; only the new one carries local: true.
  const local = [user(stamp('continue')), agent('r1'), user('continue', true)];
  assert.deepEqual(uncommittedLocalTail([stamp('continue')], local), [user('continue', true)]);
});

test('no locally-composed prompt means nothing to keep', () => {
  assert.deepEqual(uncommittedLocalTail([], [user(stamp('old')), agent('x')]), []);
  assert.deepEqual(uncommittedLocalTail([], []), []);
});

test('known limit: a prompt another client already committed hides the same prompt in flight here', () => {
  // Another browser, device or API caller committed "continue" to this
  // conversation; this browser then sent its own "continue" and reloaded while
  // that turn is still running. The snapshot holds one occurrence and so does
  // the local copy, so the counting rule sees nothing uncommitted and the
  // in-flight prompt is dropped. The client cannot tell who committed a row;
  // only a turn id from the server can. Pinned so it is not changed by accident.
  const local = [user('continue', true)];
  assert.deepEqual(uncommittedLocalTail([stamp('continue')], local), []);
});

test('known limit: a repeated prompt is lost when the local copy was truncated below the server count', () => {
  // The server holds 4 committed "continue" rows, but localStorage keeps only
  // the newest 100 bubbles, so the browser holds 3 committed ones plus the
  // in-flight one (4 occurrences, as many as the snapshot). Without a turn id or a reliable created_at the client
  // cannot tell "repeated and committed" from "repeated and in flight", so the
  // tail is empty and the prompt is dropped, exactly as before the fix. This
  // pins the limit so it is not changed by accident; only a server-side commit
  // of the prompt on accept removes it.
  const committed = [stamp('continue'), stamp('continue'), stamp('continue'), stamp('continue')];
  const local = [
    user('continue', true),
    agent('r1'),
    user('continue', true),
    agent('r2'),
    user('continue', true),
    agent('r3'),
    user('continue', true),
  ];
  assert.deepEqual(uncommittedLocalTail(committed, local), []);
});
