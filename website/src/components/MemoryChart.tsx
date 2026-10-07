import { memoryScenarios } from '../data/memory';
import { mediaFrame } from './Section';

const largestMib = Math.max(...memoryScenarios.flatMap((scenario) => scenario.results.map((result) => result.mib)));

const formatMib = (mib: number) => `${Math.round(mib).toLocaleString('en-US')} MiB`;

export function MemoryChart() {
  return (
    <div className={`${mediaFrame} flex items-center card-raised px-[6%] py-10 max-sm:px-4 max-sm:py-6`}>
      <figure className="m-0 flex w-full flex-col gap-7" aria-label="Memory use in MiB, lower is better">
        {memoryScenarios.map((scenario) => (
          <div key={scenario.title} className="grid gap-2.5">
            <h3 className="mb-0.5 text-[15px] font-medium text-secondary">{scenario.title}</h3>
            {scenario.results.map((result) => {
              const isWu = result.editor === 'Wu';
              const tone = isWu ? 'font-semibold' : 'text-secondary';
              return (
                <div
                  key={result.editor}
                  className="grid grid-cols-[72px_1fr_84px] items-center gap-3 text-[15px] max-sm:grid-cols-[60px_1fr_72px] max-sm:gap-2 max-sm:text-sm"
                  title={`${result.editor}: ${result.mib} MiB`}
                >
                  <span className={tone}>{result.editor}</span>
                  <span className="flex h-[22px]">
                    <span
                      className={`rounded-r ${isWu ? 'bg-accent' : 'bg-elevated/20'}`}
                      style={{ width: `${(result.mib / largestMib) * 100}%` }}
                    />
                  </span>
                  <span className={`text-right tabular-nums ${tone}`}>{formatMib(result.mib)}</span>
                </div>
              );
            })}
          </div>
        ))}
        <figcaption className="text-[13px] leading-[1.4] text-tertiary">
          Total process memory, lower is better.
        </figcaption>
      </figure>
    </div>
  );
}
