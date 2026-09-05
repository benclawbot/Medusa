export function appendBounded<T>(items: readonly T[], item: T, limit: number): T[] {
  if (limit <= 0) return [];
  const next = [...items, item];
  return next.length > limit ? next.slice(next.length - limit) : next;
}

export function updateBoundedById<T extends { id: string }>(
  items: readonly T[],
  item: T,
  limit: number,
): T[] {
  if (limit <= 0) return [];
  const index = items.findIndex((current) => current.id === item.id);
  if (index < 0) return appendBounded(items, item, limit);
  const next = [...items];
  next[index] = item;
  return next;
}
