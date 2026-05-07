/// 006-management-web-ui T051: NDJSON export helpers used by the
/// Audit Log page's "Download as JSON" button. NDJSON = one JSON
/// object per line, no array brackets, terminated by a trailing
/// newline so the file ends cleanly.

export function toNdjson<T>(rows: T[]): string {
  if (rows.length === 0) return "";
  return rows.map((r) => JSON.stringify(r)).join("\n") + "\n";
}

export function toNdjsonBlob<T>(rows: T[]): Blob {
  return new Blob([toNdjson(rows)], { type: "application/x-ndjson" });
}

export function downloadBlob(blob: Blob, filename: string): void {
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}
