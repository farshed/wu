import { RELEASES_URL } from '../consts';

export type Platform = 'mac' | 'windows' | 'linux';

export type MacArchitecture = 'arm' | 'x86';

export interface Download {
  label: string;
  href: string;
  platform?: Platform;
  architecture?: MacArchitecture;
}

const latest = (file: string) => `${RELEASES_URL}/latest/download/${file}`;

export const downloads: Download[] = [
  { label: 'macOS (Apple Silicon)', href: latest('Wu-aarch64.dmg'), platform: 'mac', architecture: 'arm' },
  { label: 'macOS (Intel)', href: latest('Wu-x86_64.dmg'), platform: 'mac', architecture: 'x86' },
  { label: 'Windows (x86-64)', href: latest('Wu-x86_64.exe'), platform: 'windows' },
  { label: 'Linux (x86-64)', href: latest('wu-linux-x86_64.tar.gz'), platform: 'linux' },
  { label: 'Linux (AArch64)', href: latest('wu-linux-aarch64.tar.gz') }
];

export const platformNames: Record<Platform, string> = {
  mac: 'macOS',
  windows: 'Windows',
  linux: 'Linux'
};

export function isSuggested(download: Download, platform: Platform | null, macArchitecture: MacArchitecture): boolean {
  if (!platform || download.platform !== platform) return false;
  return platform !== 'mac' || download.architecture === macArchitecture;
}

/** Platforms whose build can be picked for the visitor, so a download button can link to the file directly. */
export function directDownload(platform: Platform | null, macArchitecture: MacArchitecture): Download | undefined {
  if (platform !== 'mac' && platform !== 'windows') return undefined;
  return downloads.find((download) => isSuggested(download, platform, macArchitecture));
}

type HighEntropyUserAgentData = { getHighEntropyValues?: (hints: string[]) => Promise<{ architecture?: string }> };

/** Falls back to Apple Silicon when the browser gives no usable hint. */
export async function detectMacArchitecture(): Promise<MacArchitecture> {
  const userAgentData = (navigator as Navigator & { userAgentData?: HighEntropyUserAgentData }).userAgentData;
  if (userAgentData?.getHighEntropyValues) {
    try {
      const { architecture } = await userAgentData.getHighEntropyValues(['architecture']);
      if (architecture === 'arm' || architecture === 'x86') return architecture;
    } catch (error) {
      console.debug('Could not read the CPU architecture', error);
    }
  }
  const gl = document.createElement('canvas').getContext('webgl');
  if (!gl) return 'arm';
  const debugInfo = gl.getExtension('WEBGL_debug_renderer_info');
  const renderer = debugInfo ? String(gl.getParameter(debugInfo.UNMASKED_RENDERER_WEBGL)) : '';
  // Only Apple Silicon GPUs support ASTC textures, and Safari hides the GPU's name.
  const hasAstc = Boolean(gl.getExtension('WEBGL_compressed_texture_astc'));
  gl.getExtension('WEBGL_lose_context')?.loseContext();
  if (/apple m\d/i.test(renderer)) return 'arm';
  if (/intel|amd|radeon|nvidia/i.test(renderer)) return 'x86';
  return hasAstc ? 'arm' : 'x86';
}

export function detectPlatform(): Platform | null {
  const userAgentData = (navigator as Navigator & { userAgentData?: { platform: string } }).userAgentData;
  const platform = (userAgentData?.platform || navigator.platform || navigator.userAgent).toLowerCase();
  if (platform.includes('mac')) return 'mac';
  if (platform.includes('win')) return 'windows';
  if (platform.includes('linux') && !/android/i.test(navigator.userAgent)) return 'linux';
  return null;
}
