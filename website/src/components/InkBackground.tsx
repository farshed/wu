import { useEffect, useRef } from 'react';

const vertexSource = `
attribute vec2 position;
void main() {
  gl_Position = vec4(position, 0.0, 1.0);
}
`;

const fragmentSource = `
precision highp float;

uniform vec2 resolution;
uniform float time;
uniform vec3 ink;

float hash(vec2 point) {
  point = fract(point * vec2(123.34, 456.21));
  point += dot(point, point + 45.32);
  return fract(point.x * point.y);
}

float noise(vec2 point) {
  vec2 cell = floor(point);
  vec2 local = fract(point);
  vec2 blend = local * local * (3.0 - 2.0 * local);
  return mix(
    mix(hash(cell), hash(cell + vec2(1.0, 0.0)), blend.x),
    mix(hash(cell + vec2(0.0, 1.0)), hash(cell + vec2(1.0, 1.0)), blend.x),
    blend.y
  );
}

float fbm(vec2 point) {
  float value = 0.0;
  float amplitude = 0.5;
  mat2 rotation = mat2(1.6, 1.2, -1.2, 1.6);
  for (int octave = 0; octave < 5; octave++) {
    value += amplitude * noise(point);
    point = rotation * point;
    amplitude *= 0.5;
  }
  return value;
}

void main() {
  vec2 uv = gl_FragCoord.xy / resolution.y * 1.6;
  float t = time * 0.025;

  vec2 drift = vec2(
    fbm(uv + vec2(0.0, t)),
    fbm(uv + vec2(5.2, 1.3) - t)
  );
  vec2 swirl = vec2(
    fbm(uv + 3.6 * drift + vec2(1.7, 9.2) + 0.7 * t),
    fbm(uv + 3.6 * drift + vec2(8.3, 2.8) - 0.5 * t)
  );
  float field = fbm(uv + 3.2 * swirl);

  float body = smoothstep(0.5, 0.88, field);
  float texture = 0.55 + 0.45 * fbm(uv * 5.0 + 2.0 * swirl);
  float filament = 1.0 - smoothstep(0.0, 0.018, abs(field - 0.47));
  float density = clamp(body * texture + filament * 0.28 * smoothstep(0.35, 0.6, swirl.x), 0.0, 1.0);

  float across = gl_FragCoord.x / resolution.x;
  float wide = step(1.3, resolution.x / resolution.y);
  float mask = mix(0.5 * smoothstep(0.55, 1.0, across), smoothstep(0.3, 0.8, across), wide);

  float alpha = density * mask * 0.6;
  gl_FragColor = vec4(ink * alpha, alpha);
}
`;

const RESOLUTION_SCALE = 0.6;

function readInkColor(): [number, number, number] {
  const hex = getComputedStyle(document.documentElement).getPropertyValue('--elevated').trim().replace('#', '');
  return [0, 2, 4].map((offset) => parseInt(hex.slice(offset, offset + 2), 16) / 255) as [number, number, number];
}

function compile(gl: WebGLRenderingContext, type: number, source: string): WebGLShader | null {
  const shader = gl.createShader(type);
  if (!shader) return null;
  gl.shaderSource(shader, source);
  gl.compileShader(shader);
  if (gl.getShaderParameter(shader, gl.COMPILE_STATUS)) return shader;
  console.error(gl.getShaderInfoLog(shader));
  gl.deleteShader(shader);
  return null;
}

function createProgram(gl: WebGLRenderingContext): WebGLProgram | null {
  const vertex = compile(gl, gl.VERTEX_SHADER, vertexSource);
  const fragment = compile(gl, gl.FRAGMENT_SHADER, fragmentSource);
  const program = gl.createProgram();
  if (!vertex || !fragment || !program) return null;
  gl.attachShader(program, vertex);
  gl.attachShader(program, fragment);
  gl.linkProgram(program);
  if (gl.getProgramParameter(program, gl.LINK_STATUS)) return program;
  console.error(gl.getProgramInfoLog(program));
  return null;
}

export function InkBackground() {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    const gl = canvas?.getContext('webgl', { antialias: false, premultipliedAlpha: true });
    if (!canvas || !gl) return;
    const program = createProgram(gl);
    if (!program) return;

    gl.useProgram(program);
    gl.bindBuffer(gl.ARRAY_BUFFER, gl.createBuffer());
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 3, -1, -1, 3]), gl.STATIC_DRAW);
    const position = gl.getAttribLocation(program, 'position');
    gl.enableVertexAttribArray(position);
    gl.vertexAttribPointer(position, 2, gl.FLOAT, false, 0, 0);

    const resolutionUniform = gl.getUniformLocation(program, 'resolution');
    const timeUniform = gl.getUniformLocation(program, 'time');
    const inkUniform = gl.getUniformLocation(program, 'ink');

    const reducedMotion = matchMedia('(prefers-reduced-motion: reduce)');

    let elapsedSeconds = Math.random() * 400;
    let lastFrameTime: number | null = null;
    let animationFrame = 0;
    let onScreen = true;

    const draw = () => {
      gl.uniform2f(resolutionUniform, canvas.width, canvas.height);
      gl.uniform1f(timeUniform, elapsedSeconds);
      gl.drawArrays(gl.TRIANGLES, 0, 3);
      canvas.style.opacity = '1';
    };

    const tick = (now: number) => {
      if (lastFrameTime !== null) elapsedSeconds += (now - lastFrameTime) / 1000;
      lastFrameTime = now;
      draw();
      animationFrame = requestAnimationFrame(tick);
    };

    const updatePlayback = () => {
      cancelAnimationFrame(animationFrame);
      lastFrameTime = null;
      if (onScreen && !document.hidden && !reducedMotion.matches) {
        animationFrame = requestAnimationFrame(tick);
      } else {
        draw();
      }
    };

    const resize = () => {
      const scale = Math.min(devicePixelRatio, 2) * RESOLUTION_SCALE;
      canvas.width = Math.max(1, Math.round(canvas.clientWidth * scale));
      canvas.height = Math.max(1, Math.round(canvas.clientHeight * scale));
      gl.viewport(0, 0, canvas.width, canvas.height);
      draw();
    };

    const resizeObserver = new ResizeObserver(resize);
    const intersectionObserver = new IntersectionObserver(([entry]) => {
      onScreen = entry?.isIntersecting ?? true;
      updatePlayback();
    });

    gl.uniform3f(inkUniform, ...readInkColor());
    resize();
    resizeObserver.observe(canvas);
    intersectionObserver.observe(canvas);
    document.addEventListener('visibilitychange', updatePlayback);
    reducedMotion.addEventListener('change', updatePlayback);

    return () => {
      cancelAnimationFrame(animationFrame);
      resizeObserver.disconnect();
      intersectionObserver.disconnect();
      document.removeEventListener('visibilitychange', updatePlayback);
      reducedMotion.removeEventListener('change', updatePlayback);
    };
  }, []);

  return (
    <canvas
      ref={canvasRef}
      aria-hidden="true"
      className="pointer-events-none absolute inset-0 -z-10 size-full opacity-0 transition-opacity duration-1500 motion-reduce:transition-none"
    />
  );
}
