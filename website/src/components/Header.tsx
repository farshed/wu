import { Download } from 'lucide-react';
import { REPO_URL } from '../consts';
import { AppIcon } from './icons';

const glassPill =
  'pointer-events-auto rounded-full bg-background/72 ring-1 ring-elevated/10 backdrop-blur-md backdrop-saturate-110';

export function Header({ downloadHref = '/#download' }: { downloadHref?: string }) {
  return (
    <header className="pointer-events-none fixed inset-x-0 top-0 z-10 px-8 pt-4 max-sm:px-4 max-sm:pt-2.5">
      <div className="mx-auto flex max-w-[1240px] items-center justify-between gap-2">
        <a
          className={`${glassPill} inline-flex items-center gap-2.5 py-[5px] pr-4 pl-[5px] text-lg font-semibold no-underline`}
          href="/"
        >
          <AppIcon className="size-8 rounded-full" />
          Wu
        </a>
        <nav className={`${glassPill} flex gap-0.5 p-1`}>
          <a className="btn btn-md btn-ghost" href="/docs/">
            Docs
          </a>
          <a className="btn btn-md btn-ghost max-sm:hidden" href={REPO_URL} target="_blank" rel="noopener">
            GitHub
          </a>
          <a className="btn btn-md btn-primary" href={downloadHref}>
            <Download size={18} aria-hidden="true" />
            Download
          </a>
        </nav>
      </div>
    </header>
  );
}
