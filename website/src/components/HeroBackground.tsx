import { useEffect, useState } from 'react';
import { FluidBackground } from './FluidBackground';
import { InkBackground } from './InkBackground';

type Effect = 'interactive' | 'auto' | 'ink';

export function HeroBackground() {
  const [effect, setEffect] = useState<Effect | null>(null);

  useEffect(() => {
    if (matchMedia('(prefers-reduced-motion: reduce)').matches) setEffect('ink');
    else setEffect(matchMedia('(hover: none) and (pointer: coarse)').matches ? 'auto' : 'interactive');
  }, []);

  if (effect === null) return null;
  if (effect === 'ink') return <InkBackground />;
  return <FluidBackground auto={effect === 'auto'} onUnavailable={() => setEffect('ink')} />;
}
