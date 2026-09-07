// The private access key remains in memory for this app session; never put it in URLs or builds.
let accessKey = "";
export function getAccessKey(): string { return accessKey; }
export function setAccessKey(key: string): void { accessKey = key; }
export function authenticatedFetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
  const headers = new Headers(init?.headers);
  if (accessKey) headers.set("Authorization", `Bearer ${accessKey}`);
  return fetch(input, { ...init, headers });
}
