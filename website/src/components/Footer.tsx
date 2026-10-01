import {
  INSTALL_GUIDE_URL,
  ISSUES_URL,
  MEMORY_BENCHMARK_URL,
  RAYCAST_URL,
  RELEASES_URL,
  REPO_URL,
  ZED_URL
} from '../consts';
import { AppIcon } from './icons';

const columns = [
  {
    title: 'Wu',
    links: [
      { label: 'Download', href: '/#download' },
      { label: 'Docs', href: '/docs/' },
      { label: 'Releases', href: RELEASES_URL }
    ]
  },
  {
    title: 'Resources',
    links: [
      { label: 'Install guide', href: INSTALL_GUIDE_URL },
      { label: 'Memory benchmark', href: MEMORY_BENCHMARK_URL },
      { label: 'Raycast extension', href: RAYCAST_URL }
    ]
  },
  {
    title: 'Source',
    links: [
      { label: 'GitHub', href: REPO_URL },
      { label: 'Report an issue', href: ISSUES_URL },
    ]
  }
];

export function Footer() {
  return (
    <footer className="mx-auto flex max-w-[1240px] flex-col items-center gap-12 px-8 pt-24 pb-12 max-sm:px-4">
      <nav className="flex flex-wrap justify-center gap-x-[clamp(32px,10vw,120px)] gap-y-8" aria-label="Footer">
        {columns.map((column) => (
          <div key={column.title} className="flex min-w-[120px] flex-col gap-2.5">
            <div className="mb-1 text-sm text-tertiary">{column.title}</div>
            {column.links.map((link) => (
              <a
                key={link.label}
                className="text-[15px] text-secondary no-underline hover:underline"
                href={link.href}
                {...(link.href.startsWith('http') && { target: '_blank', rel: 'noopener' })}
              >
                {link.label}
              </a>
            ))}
          </div>
        ))}
      </nav>
      <div className="flex items-center gap-2.5 text-center text-sm text-tertiary">
        <AppIcon className="size-7 rounded-[24%]" />
        Wu is licensed under GPL-3.0-or-later.
      </div>
    </footer>
  );
}
