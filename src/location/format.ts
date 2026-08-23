// Human-readable size in decimal units; empty for directories (null bytes).
export function formatSize(bytes: number | null): string {
  if (bytes === null) {
    return "";
  }
  if (bytes < 1000) {
    return `${String(bytes)} B`;
  }
  let value = bytes / 1000;
  for (const unit of ["KB", "MB", "GB", "TB"]) {
    if (value < 1000) {
      const rounded = value < 10 ? value.toFixed(1) : String(Math.round(value));
      return `${rounded} ${unit}`;
    }
    value /= 1000;
  }
  return `${String(Math.round(value))} PB`;
}

const dateFormatter = new Intl.DateTimeFormat(undefined, {
  year: "numeric",
  month: "short",
  day: "numeric",
});

// Short human date; empty when the timestamp is unavailable.
export function formatModified(modifiedMs: number | null): string {
  if (modifiedMs === null) {
    return "";
  }
  return dateFormatter.format(new Date(modifiedMs));
}
