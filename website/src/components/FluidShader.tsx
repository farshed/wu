import { useEffect, useRef, useState } from 'react';
import { InkFlow, Shader, Stone } from 'shaders/react';

export default function FluidShader({ onUnavailable }: { onUnavailable: () => void }) {
  const frameRef = useRef<HTMLDivElement>(null);
  const [side, setSide] = useState(0);

  useEffect(() => {
    const frame = frameRef.current;
    if (!frame) return;
    // The fluid simulates on a square grid, so a non-square canvas stretches the ink.
    const observer = new ResizeObserver(() => setSide(Math.max(frame.clientWidth, frame.clientHeight)));
    observer.observe(frame);
    return () => observer.disconnect();
  }, []);

  return (
    <div ref={frameRef} aria-hidden="true" className="pointer-events-none absolute inset-0 -z-10 overflow-hidden">
      {side > 0 && (
        <Shader
          disableTelemetry
          className="absolute top-1/2 left-1/2 -translate-1/2"
          style={{ width: side, height: side }}
          onUnavailable={onUnavailable}
        >
          <Stone intensity={1} scale={200}>
            <InkFlow colorMode="custom" color1="#2216f7" color2="#55bdff" color3="#a702ff" />
          </Stone>
        </Shader>
      )}
    </div>
  );
}
