import { useEffect, useState } from 'react';
import { FluidBackground } from './FluidBackground';
import { InkBackground } from './InkBackground';

type Effect = 'fluid' | 'ink';

export function HeroBackground() {
  const [effect, setEffect] = useState<Effect | null>(null);

  useEffect(() => {
    setEffect(matchMedia('(prefers-reduced-motion: reduce)').matches ? 'ink' : 'fluid');
  }, []);

  if (effect === null) return null;
  if (effect === 'ink') return <InkBackground />;
  return <FluidBackground onUnavailable={() => setEffect('ink')} />;
}
