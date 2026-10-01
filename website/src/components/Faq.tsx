import { ChevronDown } from 'lucide-react';
import type { ReactNode } from 'react';
import { INSTALL_GUIDE_URL, ISSUES_URL } from '../consts';

const questions: { question: string; answer: ReactNode }[] = [
  {
    question: 'Is Wu free?',
    answer: "Yes. Wu is free and open source under GPL-3.0-or-later, the same license as Zed. There's no paid tier."
  },
  {
    question: 'Does Wu collect any data?',
    answer: 'No. Wu has no telemetry, no crash reporting and no account. It never sends your usage data anywhere.'
  },
  {
    question: 'Does Wu have AI features?',
    answer:
      "No. The agent panel, inline assistant and edit predictions are removed. Use whichever agent or tool you already like, in Wu's terminal or anywhere else."
  },
  {
    question: 'Do Zed extensions work in Wu?',
    answer: 'Yes. Wu installs extensions from the Zed extension registry, and every Zed extension works.'
  },
  {
    question: 'Can I keep Zed installed too?',
    answer: (
      <>
        Yes. Wu and Zed keep separate settings, in <code>~/.config/wu</code> and <code>~/.config/zed</code>. Copy your{' '}
        <code>settings.json</code> and <code>keymap.json</code> over if you want the same setup in both.
      </>
    )
  },
  {
    question: "Why does macOS say Wu can't be opened?",
    answer: (
      <>
        Wu isn't signed with an Apple Developer certificate yet, so macOS blocks it the first time. The{' '}
        <a href={INSTALL_GUIDE_URL} target="_blank" rel="noopener">
          install guide
        </a>{' '}
        shows the one-time fix.
      </>
    )
  },
  {
    question: 'Does Wu have Vim mode?',
    answer: 'No. Vim and Helix modes are removed to keep Wu simple.'
  },
  {
    question: 'How is Wu different from Zed?',
    answer: (
      <>
        Wu removes AI, collaboration and telemetry, and changes the layout and defaults to feel like VS Code. Everything
        else works the way it does in Zed. The <a href="/docs/">docs</a> list every difference.
      </>
    )
  },
  {
    question: 'Where can I report a problem?',
    answer: (
      <>
        Open an issue on{' '}
        <a href={ISSUES_URL} target="_blank" rel="noopener">
          GitHub
        </a>
        . Please check for an existing one first.
      </>
    )
  }
];

export function Faq() {
  return (
    <section className="mx-auto my-28 flex max-w-[1240px] flex-col items-center gap-3 px-8 max-sm:my-18 max-sm:px-4">
      <p className="text-lg text-secondary">Questions people ask about Wu</p>
      <div className="grid w-[min(760px,100%)] gap-1.5">
        {questions.map(({ question, answer }) => (
          <details key={question} className="group rounded-xl bg-elevated/5">
            <summary className="flex cursor-pointer list-none items-center justify-between gap-2 rounded-xl px-5 py-[18px] text-[17px] outline-offset-2 outline-primary select-none focus-visible:outline-2 max-sm:px-[18px] max-sm:py-4 max-sm:text-base [&::-webkit-details-marker]:hidden">
              {question}
              <ChevronDown
                size={18}
                className="shrink-0 text-secondary transition-transform duration-200 group-open:rotate-180"
                aria-hidden="true"
              />
            </summary>
            <p className="pr-12 pb-[18px] pl-5 leading-[1.45] text-secondary max-sm:px-[18px] max-sm:pb-4">{answer}</p>
          </details>
        ))}
      </div>
    </section>
  );
}
