// The shapes of docs/protocol.md that the extension reads.

export interface Range {
  start: number;
  count: number;
}

export interface Limits {
  max_sessions: number;
  max_session_mem: number;
  max_total_mem: number;
  default_mem: number;
  max_page: number;
  max_lines: number;
  max_line_bytes: number;
  max_context: number;
}

export interface InitializeResult {
  protocol: number;
  server: { name: string; version: string };
  limits: Limits;
}

export type State = 'scanning' | 'ready' | 'failed';

export interface OpenResult {
  session: number;
  reused: boolean;
  mem_bytes: number;
  state: State;
}

export interface Progress {
  phase: 'scan' | 'refine';
  done: number;
  total: number;
}

export interface RpcErrorObject {
  code: number;
  message: string;
  data?: Record<string, unknown>;
}

export interface Count {
  segments: number;
  a_lines: number;
  b_lines: number;
}

export interface Summary {
  state: State;
  a: { lines?: number; bytes: number };
  b: { lines?: number; bytes: number };
  identical?: boolean;
  equal?: Count;
  differs?: Count;
  approx?: Count;
  differing_segments?: number;
  moves?: 'found' | 'off' | 'skipped';
  moved_blocks?: number;
  mem_bytes: number;
  progress?: Progress;
  error?: RpcErrorObject;
}

export type SegmentStatus = 'differs' | 'approx';

export interface Segment {
  index: number;
  a: Range;
  b: Range;
  status: SegmentStatus;
}

export interface SegmentsResult {
  total: number;
  offset: number;
  segments: Segment[];
}

export interface Line {
  text: string;
  bytes: number;
  eol: 'lf' | 'crlf' | 'none';
  truncated?: boolean;
  lossy?: boolean;
  moved?: boolean;
}

export interface Side {
  start: number;
  total: number;
  lines: Line[];
}

export interface HunkResult {
  segment: number;
  status: 'equal' | SegmentStatus;
  pending: boolean;
  range: { a: Range; b: Range };
  a: Side;
  b: Side;
  truncated?: boolean;
}

export interface MoveBlock {
  a: Range;
  b: Range;
}

export interface MovesResult {
  total: number;
  offset: number;
  moves: 'found' | 'off' | 'skipped';
  blocks: MoveBlock[];
}

export type Notification =
  | { method: 'progress'; params: { session: number } & Progress }
  | { method: 'session_state'; params: { session: number; state: 'ready' | 'failed'; error?: RpcErrorObject } }
  | { method: 'segment_ready'; params: { session: number; index: number; status: SegmentStatus } };

export const Code = {
  budgetExceeded: -32003,
  busy: -32005,
  unknownSession: -32006,
  notReady: -32008,
} as const;
