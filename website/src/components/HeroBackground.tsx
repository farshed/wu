import { lazy, Suspense, useEffect, useState } from 'react';
import { InkBackground } from './InkBackground';

const FluidShader = lazy(() => import('./FluidShader'));

type Effect = 'fluid' | 'ink';

export function HeroBackground() {
  const [effect, setEffect] = useState<Effect | null>(null);

  useEffect(() => {
    const reducedMotion = matchMedia('(prefers-reduced-motion: reduce)').matches;
    setEffect('gpu' in navigator && !reducedMotion ? 'fluid' : 'ink');
  }, []);

  if (effect === null) return null;
  if (effect === 'ink') return <InkBackground />;
  return (
    <Suspense fallback={null}>
      <FluidShader onUnavailable={() => setEffect('ink')} />
    </Suspense>
  );
}
