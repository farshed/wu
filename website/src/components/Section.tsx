import type { ReactNode } from 'react';

export const sectionTitle = 'text-[32px] leading-[1.1] font-medium tracking-[-0.025em] max-sm:text-[28px]';
export const sectionBody = 'text-lg leading-[1.4] text-secondary max-sm:text-[17px]';
export const mediaFrame =
  'relative min-w-0 flex-1 overflow-hidden rounded-xl bg-elevated/5 after:pointer-events-none after:absolute after:inset-0 after:rounded-[inherit] after:ring-1 after:ring-elevated/10 after:ring-inset';

export function MediaSection({
  text,
  media,
  mediaFirst = false
}: {
  text: ReactNode;
  media: ReactNode;
  mediaFirst?: boolean;
}) {
  return (
    <section
      className={`mx-auto my-28 flex max-w-[1240px] items-center gap-12 px-8 max-[950px]:flex-col max-[950px]:items-stretch max-[950px]:gap-7 max-sm:my-18 max-sm:px-4 ${mediaFirst ? 'flex-row-reverse' : ''}`}
    >
      <div className="flex flex-[0_0_min(440px,40%)] flex-col items-start gap-3 max-[950px]:flex-none">{text}</div>
      {media}
    </section>
  );
}

export function CenterSection({ children, id, card = false }: { children: ReactNode; id?: string; card?: boolean }) {
  return (
    <section
      id={id}
      className={
        card
          ? 'mx-1 my-28 rounded-xl bg-linear-to-b from-hero-end to-hero-start px-8 py-28 max-sm:my-18 max-sm:px-4 max-sm:py-16'
          : 'mx-auto my-28 max-w-[1240px] px-8 max-sm:my-18 max-sm:px-4'
      }
    >
      <div className="mx-auto flex max-w-[600px] flex-col items-center gap-3 text-center">{children}</div>
    </section>
  );
}
