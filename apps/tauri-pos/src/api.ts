/**
 * The only door between React and the core.
 *
 * Each function forwards to a Tauri command that is itself nothing more than an
 * `EdgeClient` call over IPC. No business logic lives on this side of the
 * boundary, and no state is cached here: when something changes, we re-ask the
 * core.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import type {
  BridgeError,
  ConnectionStatus,
  CoreInfo,
  EdgeState,
  StateChangedEvent,
} from "./types";

/** Event the Rust bridge emits when the core says its state changed. */
export const STATE_CHANGED_EVENT = "edge://state-changed";

export function isBridgeError(value: unknown): value is BridgeError {
  return (
    typeof value === "object" &&
    value !== null &&
    "code" in value &&
    "message" in value
  );
}

export function describeError(value: unknown): BridgeError {
  if (isBridgeError(value)) {
    return value;
  }
  return { code: "UNKNOWN", message: String(value) };
}

export function fetchCoreInfo(): Promise<CoreInfo> {
  return invoke<CoreInfo>("core_info");
}

export function fetchEdgeState(): Promise<EdgeState> {
  return invoke<EdgeState>("edge_state");
}

export function fetchConnection(): Promise<ConnectionStatus> {
  return invoke<ConnectionStatus>("connection_status");
}

export function onStateChanged(
  handler: (event: StateChangedEvent) => void,
): Promise<UnlistenFn> {
  return listen<StateChangedEvent>(STATE_CHANGED_EVENT, (event) => {
    handler(event.payload);
  });
}
