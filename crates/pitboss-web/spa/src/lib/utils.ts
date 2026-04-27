import { clsx, type ClassValue } from 'clsx';
import { twMerge } from 'tailwind-merge';

export function cn(...inputs: ClassValue[]): string {
  return twMerge(clsx(inputs));
}

export function formatUnixSeconds(unix: number, locale = navigator.language): string {
  if (!unix || Number.isNaN(unix)) return '—';
  const d = new Date(unix * 1000);
  return d.toLocaleString(locale, {
    year: 'numeric',
    month: 'short',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit'
  });
}

export function relativeFromUnix(unix: number, now = Date.now() / 1000): string {
  if (!unix) return '—';
  const delta = Math.max(0, now - unix);
  if (delta < 60) return `${Math.round(delta)}s ago`;
  if (delta < 3600) return `${Math.round(delta / 60)}m ago`;
  if (delta < 86400) return `${Math.round(delta / 3600)}h ago`;
  return `${Math.round(delta / 86400)}d ago`;
}
