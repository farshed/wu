import { RELEASES_URL } from '../consts';

export type Platform = 'mac' | 'windows' | 'linux';

export interface Download {
  label: string;
  href: string;
  platform?: Platform;
}

const latest = (file: string) => `${RELEASES_URL}/latest/download/${file}`;

export const downloads: Download[] = [
  { label: 'macOS (Apple Silicon)', href: latest('Wu-aarch64.dmg'), platform: 'mac' },
  { label: 'Windows (x86-64)', href: latest('Wu-x86_64.exe'), platform: 'windows' },
  { label: 'Linux (x86-64)', href: latest('wu-linux-x86_64.tar.gz'), platform: 'linux' },
  { label: 'Linux (AArch64)', href: latest('wu-linux-aarch64.tar.gz') }
];

export const platformNames: Record<Platform, string> = {
  mac: 'macOS',
  windows: 'Windows',
  linux: 'Linux'
};

/** Platforms with a single build, so a download button can link to the file directly. */
export function directDownload(platform: Platform | null): Download | undefined {
  if (platform !== 'mac' && platform !== 'windows') return undefined;
  return downloads.find((download) => download.platform === platform);
}

export function detectPlatform(): Platform | null {
  const userAgentData = (navigator as Navigator & { userAgentData?: { platform: string } }).userAgentData;
  const platform = (userAgentData?.platform || navigator.platform || navigator.userAgent).toLowerCase();
  if (platform.includes('mac')) return 'mac';
  if (platform.includes('win')) return 'windows';
  if (platform.includes('linux') && !/android/i.test(navigator.userAgent)) return 'linux';
  return null;
}
