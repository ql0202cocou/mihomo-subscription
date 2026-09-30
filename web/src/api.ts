// 极简 API 客户端。管理接口走同源 JSON;遇到 401 时触发注册的未授权回调,
// 让 SPA 跳转到登录页。

/** 管理 API 错误。网络层失败(fetch reject)不会包装成本类,catch 处用 instanceof 区分。 */
export class ApiError extends Error {
  status: number;
  code?: string;
  details?: string[];

  constructor(status: number, body: { code?: string; message?: string; details?: string[] } = {}) {
    super(body.message ?? `HTTP ${status}`);
    this.name = "ApiError";
    this.status = status;
    this.code = body.code;
    this.details = body.details;
  }
}

/** catch 处的统一取文案策略:管理 API 错误用服务端消息(构造时兜底为 `HTTP <status>`),
 *  其余异常(网络层 fetch reject 等)用调用方提供的本地化兜底文案。 */
export function errorMessage(e: unknown, fallback: string): string {
  return e instanceof ApiError ? e.message : fallback;
}

let onUnauthorized: (() => void) | null = null;

export function setUnauthorizedHandler(fn: (() => void) | null): void {
  onUnauthorized = fn;
}

export async function api<T>(path: string, opts: RequestInit = {}): Promise<T> {
  const res = await fetch(path, {
    headers: { "Content-Type": "application/json", ...(opts.headers ?? {}) },
    ...opts,
  });

  if (res.status === 401) {
    onUnauthorized?.();
    throw new ApiError(401);
  }

  if (!res.ok) {
    let body: { error?: { code?: string; message?: string; details?: string[] } } = {};
    try {
      body = await res.json();
    } catch {
      // 错误体不是 JSON;只保留状态码
    }
    throw new ApiError(res.status, body.error ?? {});
  }

  if (res.status === 204) {
    return undefined as T;
  }
  const contentType = res.headers.get("content-type") ?? "";
  if (contentType.includes("application/json")) {
    return (await res.json()) as T;
  }
  return (await res.text()) as unknown as T;
}
