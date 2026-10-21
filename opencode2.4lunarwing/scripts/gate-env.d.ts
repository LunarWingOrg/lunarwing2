/**
 * gate-env.d.ts — minimal ambient declarations for the runtime surface the
 * SDK-facing scripts use, for the WI#369 builder-stage tsgo gate only
 * (tsconfig.sdk-surface.json). The opencode monorepo's bun install scatters
 * @types across per-package node_modules and its versioned isolated store,
 * none of which is stably addressable from that tsconfig — and none of the
 * gated scripts use Bun-only globals. The gate therefore carries exactly the
 * surface it checks (self-contained, no install-layout coupling). Extend
 * this file when a gated script starts using a new runtime API; that
 * fail-closed friction is deliberate.
 */

declare namespace NodeJS {
  interface ErrnoException extends Error {
    code?: string
  }
}

declare const process: {
  env: Record<string, string | undefined>
  exit(code?: number): never
}

interface GateBuffer {
  readonly length: number
  readonly [index: number]: number
  subarray(start?: number, end?: number): GateBuffer
  toString(encoding?: string): string
}

declare const Buffer: {
  from(input: string, encoding?: string): GateBuffer
  byteLength(input: string, encoding?: string): number
}

declare module "crypto" {
  export function randomUUID(): string
}

declare module "fs" {
  export function writeFileSync(path: string, data: string): void
  export function readFileSync(path: string, encoding: string): string
}

declare module "node:fs" {
  export const constants: {
    O_RDONLY: number
    O_DIRECTORY: number
    O_NOFOLLOW: number
  }
  export function closeSync(fd: number): void
  export function existsSync(path: string): boolean
  export function fchownSync(fd: number, uid: number, gid: number): void
  export function fstatSync(fd: number): { dev: number; ino: number }
  export function mkdirSync(path: string): void
  export function openSync(path: string, flags: number): number
  export function realpathSync(path: string): string
  export function readFileSync(path: string, encoding: string): string
  export function statSync(path: string): { dev: number; ino: number }
  export function writeFileSync(path: string, data: string): void
}

declare module "node:os" {
  export function tmpdir(): string
}

declare module "node:path" {
  export function dirname(path: string): string
  export function isAbsolute(path: string): boolean
  export function join(...parts: string[]): string
  export function relative(from: string, to: string): string
  export function resolve(...parts: string[]): string
}

declare module "node:module" {
  export function createRequire(filename: string): { resolve(id: string): string }
}
