/**
 * Core JSON-RPC 2.0 protocol specifications and handler primitives.
 */

export interface JsonRpcRequest<T = unknown> {
  jsonrpc: "2.0";
  method: string;
  params?: T;
  id: number | string;
}

export interface JsonRpcNotification<T = unknown> {
  jsonrpc: "2.0";
  method: string;
  params?: T;
}

export interface JsonRpcError {
  code: number;
  message: string;
  data?: unknown;
}

export interface JsonRpcResponse<T = unknown> {
  jsonrpc: "2.0";
  id: number | string | null;
  result?: T;
  error?: JsonRpcError;
}

export type NotificationHandler = (params: unknown) => void;
export type DisconnectHandler = (error: Error) => void;

export interface IpcClientOptions {
  /**
   * Default request timeout in milliseconds. `0` disables timeouts.
   * Can be overridden per-request via `request(method, params, timeoutMs)`.
   */
  requestTimeoutMs?: number;
}
