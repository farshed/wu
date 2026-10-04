import { Pause, Play } from 'lucide-react';
import { useEffect, useState, type ReactNode } from 'react';
import { mediaFrame, sectionBody, sectionTitle } from './Section';

interface Highlight {
  title: string;
  paragraphs: string[];
  media: ReactNode;
}

const screenshot = 'size-full object-cover';

const highlights: Highlight[] = [
  {
    title: 'Native and fast',
    paragraphs: [
      "Wu is written in Rust and drawn on the GPU. There's no Electron and no web views.",
      'It opens instantly and stays responsive on large files and big projects.'
    ],
    media: <img className={screenshot} src="/screenshot-dark.png" alt="Wu editor with a dark theme" />
  },
  {
    title: 'Coding agents, built in',
    paragraphs: [
      'Chat with Claude Code or Codex right in Wu. It runs the CLIs you already have installed and signed in, so there are no API keys and no extra subscription.',
      'Queue messages, follow subagents, review diffs and switch models without leaving the editor.'
    ],
    media: (
      <div className="flex size-full flex-col justify-center gap-[1.4cqw] px-[9cqw] text-[7cqw] leading-[1.05] tracking-[-0.035em]">
        <span>Claude Code.</span>
        <span className="text-secondary">Codex.</span>
        <span className="text-tertiary">Built in.</span>
      </div>
    )
  },
  {
    title: 'Beautiful by default',
    paragraphs: [
      'A clean, modern interface that stays out of your way, in light and dark.',
      'It looks good from the first launch, so you can start working instead of tweaking settings.'
    ],
    media: <img className={screenshot} src="/screenshot-light.png" alt="Wu editor with a light theme" loading="lazy" />
  },
  {
    title: 'Yours, not ours',
    paragraphs: [
      "Wu works with the agents you already pay for. Nothing goes through a Wu server.",
      "There's nothing to sign in to, and Wu never sends your usage data anywhere."
    ],
    media: (
      <div className="flex size-full flex-col justify-center gap-[1.4cqw] px-[9cqw] text-[7cqw] leading-[1.05] tracking-[-0.035em]">
        <span>No account.</span>
        <span className="text-secondary">No telemetry.</span>
        <span className="text-tertiary">No API keys.</span>
      </div>
    )
  }
];

const stackedPanel =
  'col-start-1 row-start-1 transition-[opacity,visibility] duration-250 motion-reduce:transition-none';

const panelVisibility = (isCurrent: boolean) => (isCurrent ? 'visible opacity-100' : 'invisible opacity-0');

export function Highlights() {
  const [current, setCurrent] = useState(0);
  const [paused, setPaused] = useState(false);

  useEffect(() => {
    if (matchMedia('(prefers-reduced-motion: reduce)').matches) setPaused(true);
  }, []);

  const showNext = () => setCurrent((index) => (index + 1) % highlights.length);

  return (
    <section className="mx-auto my-28 flex max-w-[1240px] items-center gap-12 px-8 max-[950px]:flex-col max-[950px]:items-stretch max-[950px]:gap-7 max-sm:my-18 max-sm:px-4">
      <div className="flex flex-[0_0_min(440px,40%)] flex-col gap-8 max-[950px]:flex-none">
        <div className="grid">
          {highlights.map((highlight, index) => (
            <div
              key={highlight.title}
              className={`${stackedPanel} ${panelVisibility(index === current)} flex flex-col items-start gap-3 self-start`}
              aria-hidden={index !== current}
            >
              <h2 className={sectionTitle}>{highlight.title}</h2>
              {highlight.paragraphs.map((paragraph) => (
                <p key={paragraph} className={sectionBody}>
                  {paragraph}
                </p>
              ))}
            </div>
          ))}
        </div>
        <div className="flex items-center gap-1.5 [html:not(.js)_&]:hidden">
          <div className="flex h-10 items-center rounded-full bg-elevated/7 px-[11px]">
            {highlights.map((highlight, index) => {
              const isCurrent = index === current;
              return (
                <button
                  key={highlight.title}
                  className="grid h-10 cursor-pointer place-items-center px-[5px]"
                  type="button"
                  aria-label={`Show ${highlight.title}`}
                  aria-current={isCurrent}
                  onClick={() => setCurrent(index)}
                >
                  <span
                    className={`relative block h-2.5 overflow-hidden rounded-[5px] bg-elevated/20 transition-[width] duration-240 motion-reduce:transition-none ${isCurrent ? 'w-[52px]' : 'w-2.5'}`}
                  >
                    {isCurrent && (
                      <span
                        key={current}
                        className={`absolute inset-y-0 left-0 rounded-[inherit] bg-primary ${paused ? 'w-full' : 'animate-fill'}`}
                        onAnimationEnd={showNext}
                      />
                    )}
                  </span>
                </button>
              );
            })}
          </div>
          <button
            className="btn btn-soft size-10"
            type="button"
            aria-label={paused ? 'Play' : 'Pause'}
            aria-pressed={paused}
            onClick={() => setPaused(!paused)}
          >
            {paused ? (
              <Play size={18} fill="currentColor" strokeWidth={0} aria-hidden="true" />
            ) : (
              <Pause size={18} fill="currentColor" strokeWidth={0} aria-hidden="true" />
            )}
          </button>
        </div>
      </div>
      <div className={`${mediaFrame} @container grid aspect-[16/10] grid-cols-1 grid-rows-1`}>
        {highlights.map((highlight, index) => (
          <div
            key={highlight.title}
            className={`${stackedPanel} ${panelVisibility(index === current)}`}
            aria-hidden={index !== current}
          >
            {highlight.media}
          </div>
        ))}
      </div>
    </section>
  );
}
