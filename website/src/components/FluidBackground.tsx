import { useEffect, useRef } from 'react';

const CELLS_ON_LONG_SIDE = 256;
const BRUSH_RADIUS = 0.3 * 0.1 * CELLS_ON_LONG_SIDE;
const IMPULSE = 0.16;
const MAX_STAMPS = 16;
const STAMP_SPACING = Math.max(0.004 * CELLS_ON_LONG_SIDE, BRUSH_RADIUS * 0.6);
const PRESSURE_ITERATIONS = 10;
const PRESSURE_DECAY = 0.8;
const VELOCITY_FADE = 4 * Math.pow(0.05 / 4, 0.6);
const DYE_FADE = 0.5;
const CYCLE_PER_STAMP = 0.006;
const CYCLE_PER_SECOND = 0.06;
const IDLE_AFTER_MS = 10_000;
const MAX_PIXEL_RATIO = 1.5;
const COLORS = ['#2216f7', '#55bdff', '#a702ff'];

const vertexSource = `#version 300 es
in vec2 position;
void main() {
  gl_Position = vec4(position, 0.0, 1.0);
}`;

const fieldHeader = `#version 300 es
precision highp float;
precision highp sampler2D;
uniform vec2 grid;
out vec4 outColor;
ivec2 cell() { return ivec2(gl_FragCoord.xy); }
vec4 fetch(sampler2D field, ivec2 at) {
  return texelFetch(field, clamp(at, ivec2(0), ivec2(grid) - 1), 0);
}
`;

const splatSource = `${fieldHeader}
uniform sampler2D velocity;
uniform sampler2D dye;
uniform vec2 point;
uniform vec2 impulse;
uniform vec3 color;
uniform int splatTarget;
void main() {
  vec2 offset = gl_FragCoord.xy - point;
  float influence = exp(-dot(offset, offset) / ${(BRUSH_RADIUS * BRUSH_RADIUS).toFixed(4)});
  if (splatTarget == 0) {
    outColor = vec4(fetch(velocity, cell()).xy + impulse * influence, 0.0, 1.0);
  } else {
    outColor = vec4(mix(fetch(dye, cell()).rgb, color, clamp(influence, 0.0, 1.0)), 1.0);
  }
}`;

const divergenceSource = `${fieldHeader}
uniform sampler2D velocity;
void main() {
  ivec2 c = cell();
  ivec2 last = ivec2(grid) - 1;
  vec2 self = fetch(velocity, c).xy;
  float left = c.x == 0 ? -self.x : fetch(velocity, c - ivec2(1, 0)).x;
  float right = c.x == last.x ? -self.x : fetch(velocity, c + ivec2(1, 0)).x;
  float below = c.y == 0 ? -self.y : fetch(velocity, c - ivec2(0, 1)).y;
  float above = c.y == last.y ? -self.y : fetch(velocity, c + ivec2(0, 1)).y;
  outColor = vec4((right - left + above - below) * 0.5, 0.0, 0.0, 1.0);
}`;

const scaleSource = `${fieldHeader}
uniform sampler2D field;
uniform float factor;
void main() {
  outColor = fetch(field, cell()) * factor;
}`;

const jacobiSource = `${fieldHeader}
uniform sampler2D pressure;
uniform sampler2D divergence;
void main() {
  ivec2 c = cell();
  float sum = fetch(pressure, c - ivec2(1, 0)).x + fetch(pressure, c + ivec2(1, 0)).x
    + fetch(pressure, c - ivec2(0, 1)).x + fetch(pressure, c + ivec2(0, 1)).x;
  outColor = vec4((sum - fetch(divergence, c).x) * 0.25, 0.0, 0.0, 1.0);
}`;

const gradientSource = `${fieldHeader}
uniform sampler2D velocity;
uniform sampler2D pressure;
void main() {
  ivec2 c = cell();
  vec2 gradient = vec2(
    fetch(pressure, c + ivec2(1, 0)).x - fetch(pressure, c - ivec2(1, 0)).x,
    fetch(pressure, c + ivec2(0, 1)).x - fetch(pressure, c - ivec2(0, 1)).x
  ) * 0.5;
  outColor = vec4(fetch(velocity, c).xy - gradient, 0.0, 1.0);
}`;

const advectSource = `${fieldHeader}
uniform sampler2D velocity;
uniform sampler2D field;
uniform float dt;
uniform float fade;
uniform float floorAtZero;
void main() {
  vec2 source = gl_FragCoord.xy - fetch(velocity, cell()).xy * dt;
  source = clamp(source, vec2(0.5), grid - 0.5);
  vec3 value = texture(field, source / grid).rgb / (1.0 + fade * dt);
  outColor = vec4(mix(value, max(value, 0.0), floorAtZero), 1.0);
}`;

const displaySource = `#version 300 es
precision highp float;
uniform sampler2D dye;
uniform vec2 resolution;
uniform float pixelRatio;
out vec4 outColor;

vec2 hash22(vec2 p) {
  vec3 q = fract(vec3(p.xyx) * vec3(0.1031, 0.1030, 0.0973));
  q += dot(q, q.yzx + 33.33);
  return fract((q.xx + q.yz) * q.zy);
}

vec3 perlinWithGradient(vec2 p) {
  vec2 i = floor(p);
  vec2 f = fract(p);
  vec2 u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
  vec2 du = 30.0 * f * f * (f * (f - 2.0) + 1.0);
  vec2 ga = hash22(i) * 2.0 - 1.0;
  vec2 gb = hash22(i + vec2(1.0, 0.0)) * 2.0 - 1.0;
  vec2 gc = hash22(i + vec2(0.0, 1.0)) * 2.0 - 1.0;
  vec2 gd = hash22(i + vec2(1.0, 1.0)) * 2.0 - 1.0;
  float va = dot(ga, f);
  float vb = dot(gb, f - vec2(1.0, 0.0));
  float vc = dot(gc, f - vec2(0.0, 1.0));
  float vd = dot(gd, f - vec2(1.0, 1.0));
  float k = va - vb - vc + vd;
  float value = va + u.x * (vb - va) + u.y * (vc - va) + u.x * u.y * k;
  vec2 gradient = ga + u.x * (gb - ga) + u.y * (gc - ga) + u.x * u.y * (ga - gb - gc + gd)
    + du * (u.yx * k + vec2(vb, vc) - va);
  return vec3(value, gradient);
}

float stone(vec2 p) {
  vec3 warp = vec3(0.0);
  float amplitude = 1.0;
  vec2 q = p;
  for (int octave = 0; octave < 3; octave++) {
    warp += perlinWithGradient(q) * amplitude;
    amplitude *= 0.5;
    q *= 2.0;
  }
  float sum = 0.0;
  float weight = 0.0;
  amplitude = 1.0;
  q = p + warp.yz * 0.4;
  for (int octave = 0; octave < 3; octave++) {
    sum += amplitude * (perlinWithGradient(q).x * 0.7 + 0.5);
    weight += amplitude;
    amplitude *= 0.5;
    q *= 2.0;
  }
  return sum / weight;
}

vec3 toSrgb(vec3 linear) {
  return mix(linear * 12.92, 1.055 * pow(linear, vec3(1.0 / 2.4)) - 0.055, step(0.0031308, linear));
}

void main() {
  vec2 uv = gl_FragCoord.xy / resolution;
  vec2 grain = gl_FragCoord.xy / pixelRatio * 0.6;
  vec2 displaced = uv + perlinWithGradient(grain).yz * (0.15 * 0.012);
  vec3 ink = texture(dye, displaced).rgb;
  float alpha = clamp(max(ink.r, max(ink.g, ink.b)) * 1.35, 0.0, 1.0);
  if (alpha < 0.002) {
    outColor = vec4(0.0);
    return;
  }
  float brightness = clamp(1.0 + (stone(grain) - 0.5) * 1.1, 0.0, 1.6);
  outColor = vec4(toSrgb(ink * brightness) * alpha, alpha);
}`;

type Rgb = [number, number, number];

function hexToLinear(hex: string): Rgb {
  return [1, 3, 5].map((offset) => {
    const channel = parseInt(hex.slice(offset, offset + 2), 16) / 255;
    return channel <= 0.04045 ? channel / 12.92 : Math.pow((channel + 0.055) / 1.055, 2.4);
  }) as Rgb;
}

function linearToOklab([r, g, b]: Rgb): Rgb {
  const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
  const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
  const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
  return [
    0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
    1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
    0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s
  ];
}

function oklabToLinear([lightness, a, b]: Rgb): Rgb {
  const l = (lightness + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m = (lightness - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s = (lightness - 0.0894841775 * a - 1.291485548 * b) ** 3;
  return [
    4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
    -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
    -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s
  ].map((channel) => Math.max(channel, 0)) as Rgb;
}

const colorStops = COLORS.map((hex) => linearToOklab(hexToLinear(hex)));

function cycleColor(position: number): Rgb {
  const scaled = (((position % 1) + 1) % 1) * colorStops.length;
  const segment = Math.min(colorStops.length - 1, Math.floor(scaled));
  const t = scaled - segment;
  const [fromL, fromA, fromB] = colorStops[segment] ?? [0, 0, 0];
  const [toL, toA, toB] = colorStops[(segment + 1) % colorStops.length] ?? [0, 0, 0];
  return oklabToLinear([fromL + (toL - fromL) * t, fromA + (toA - fromA) * t, fromB + (toB - fromB) * t]);
}

type Program = { program: WebGLProgram; uniforms: Map<string, WebGLUniformLocation | null> };
type Target = { texture: WebGLTexture; framebuffer: WebGLFramebuffer };
type PingPong = { read: Target; write: Target; swap: () => void };

function compileProgram(gl: WebGL2RenderingContext, fragmentSource: string): Program | null {
  const compile = (type: number, source: string) => {
    const shader = gl.createShader(type);
    if (!shader) return null;
    gl.shaderSource(shader, source);
    gl.compileShader(shader);
    if (gl.getShaderParameter(shader, gl.COMPILE_STATUS)) return shader;
    console.error(gl.getShaderInfoLog(shader));
    return null;
  };
  const vertex = compile(gl.VERTEX_SHADER, vertexSource);
  const fragment = compile(gl.FRAGMENT_SHADER, fragmentSource);
  const program = gl.createProgram();
  if (!vertex || !fragment || !program) return null;
  gl.attachShader(program, vertex);
  gl.attachShader(program, fragment);
  gl.bindAttribLocation(program, 0, 'position');
  gl.linkProgram(program);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    console.error(gl.getProgramInfoLog(program));
    return null;
  }
  const uniforms = new Map<string, WebGLUniformLocation | null>();
  const count = gl.getProgramParameter(program, gl.ACTIVE_UNIFORMS) as number;
  for (let index = 0; index < count; index++) {
    const info = gl.getActiveUniform(program, index);
    if (info) uniforms.set(info.name, gl.getUniformLocation(program, info.name));
  }
  return { program, uniforms };
}

function createTarget(gl: WebGL2RenderingContext, width: number, height: number): Target | null {
  const texture = gl.createTexture();
  const framebuffer = gl.createFramebuffer();
  if (!texture || !framebuffer) return null;
  gl.bindTexture(gl.TEXTURE_2D, texture);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA16F, width, height, 0, gl.RGBA, gl.HALF_FLOAT, null);
  gl.bindFramebuffer(gl.FRAMEBUFFER, framebuffer);
  gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, texture, 0);
  gl.clearColor(0, 0, 0, 1);
  gl.clear(gl.COLOR_BUFFER_BIT);
  if (gl.checkFramebufferStatus(gl.FRAMEBUFFER) !== gl.FRAMEBUFFER_COMPLETE) return null;
  return { texture, framebuffer };
}

function createPingPong(gl: WebGL2RenderingContext, width: number, height: number): PingPong | null {
  const first = createTarget(gl, width, height);
  const second = createTarget(gl, width, height);
  if (!first || !second) return null;
  const pair: PingPong = {
    read: first,
    write: second,
    swap: () => {
      [pair.read, pair.write] = [pair.write, pair.read];
    }
  };
  return pair;
}

export function FluidBackground({ onUnavailable }: { onUnavailable: () => void }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const unavailableRef = useRef(onUnavailable);
  unavailableRef.current = onUnavailable;

  useEffect(() => {
    const onUnavailable = () => unavailableRef.current();
    const canvas = canvasRef.current;
    const gl = canvas?.getContext('webgl2', { alpha: true, antialias: false, premultipliedAlpha: true });
    if (!canvas || !gl || !(gl.getExtension('EXT_color_buffer_float') ?? gl.getExtension('EXT_color_buffer_half_float'))) {
      onUnavailable();
      return;
    }

    const programs = {
      splat: compileProgram(gl, splatSource),
      divergence: compileProgram(gl, divergenceSource),
      scale: compileProgram(gl, scaleSource),
      jacobi: compileProgram(gl, jacobiSource),
      gradient: compileProgram(gl, gradientSource),
      advect: compileProgram(gl, advectSource),
      display: compileProgram(gl, displaySource)
    };
    if (Object.values(programs).some((program) => !program)) {
      onUnavailable();
      return;
    }
    const { splat, divergence, scale, jacobi, gradient, advect, display } = programs as Record<keyof typeof programs, Program>;

    gl.bindBuffer(gl.ARRAY_BUFFER, gl.createBuffer());
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 3, -1, -1, 3]), gl.STATIC_DRAW);
    gl.enableVertexAttribArray(0);
    gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);

    let gridWidth = 0;
    let gridHeight = 0;
    let velocity: PingPong | null = null;
    let dye: PingPong | null = null;
    let pressure: PingPong | null = null;
    let divergenceTarget: Target | null = null;

    const allocate = (width: number, height: number) => {
      const longSide = Math.max(width, height, 1);
      const nextWidth = Math.max(1, Math.round((CELLS_ON_LONG_SIDE * width) / longSide));
      const nextHeight = Math.max(1, Math.round((CELLS_ON_LONG_SIDE * height) / longSide));
      if (nextWidth === gridWidth && nextHeight === gridHeight) return true;
      gridWidth = nextWidth;
      gridHeight = nextHeight;
      velocity = createPingPong(gl, gridWidth, gridHeight);
      dye = createPingPong(gl, gridWidth, gridHeight);
      pressure = createPingPong(gl, gridWidth, gridHeight);
      divergenceTarget = createTarget(gl, gridWidth, gridHeight);
      return Boolean(velocity && dye && pressure && divergenceTarget);
    };

    const use = ({ program, uniforms }: Program, textures: Record<string, WebGLTexture>) => {
      gl.useProgram(program);
      Object.entries(textures).forEach(([name, texture], unit) => {
        gl.activeTexture(gl.TEXTURE0 + unit);
        gl.bindTexture(gl.TEXTURE_2D, texture);
        gl.uniform1i(uniforms.get(name) ?? null, unit);
      });
      gl.uniform2f(uniforms.get('grid') ?? null, gridWidth, gridHeight);
      return uniforms;
    };

    const drawInto = (target: Target) => {
      gl.bindFramebuffer(gl.FRAMEBUFFER, target.framebuffer);
      gl.viewport(0, 0, gridWidth, gridHeight);
      gl.drawArrays(gl.TRIANGLES, 0, 3);
    };

    let colorCycle = 0;
    const pendingStamps: { x: number; y: number; impulseX: number; impulseY: number }[] = [];
    let pointer: { x: number; y: number; time: number } | null = null;
    let smoothedVelocity = { x: 0, y: 0 };

    const stamp = (x: number, y: number, impulseX: number, impulseY: number) => {
      if (!velocity || !dye) return;
      colorCycle += CYCLE_PER_STAMP;
      const color = cycleColor(colorCycle);
      for (const [target, field] of [
        [0, velocity],
        [1, dye]
      ] as const) {
        const uniforms = use(splat, { velocity: velocity.read.texture, dye: dye.read.texture });
        gl.uniform2f(uniforms.get('point') ?? null, x, y);
        gl.uniform2f(uniforms.get('impulse') ?? null, impulseX, impulseY);
        gl.uniform3f(uniforms.get('color') ?? null, ...color);
        gl.uniform1i(uniforms.get('splatTarget') ?? null, target);
        drawInto(field.write);
        field.swap();
      }
    };

    const step = (dt: number) => {
      if (!velocity || !dye || !pressure || !divergenceTarget) return;
      for (const pending of pendingStamps.splice(0)) stamp(pending.x, pending.y, pending.impulseX, pending.impulseY);

      use(divergence, { velocity: velocity.read.texture });
      drawInto(divergenceTarget);

      gl.uniform1f(use(scale, { field: pressure.read.texture }).get('factor') ?? null, PRESSURE_DECAY);
      drawInto(pressure.write);
      pressure.swap();
      for (let iteration = 0; iteration < PRESSURE_ITERATIONS; iteration++) {
        use(jacobi, { pressure: pressure.read.texture, divergence: divergenceTarget.texture });
        drawInto(pressure.write);
        pressure.swap();
      }

      use(gradient, { velocity: velocity.read.texture, pressure: pressure.read.texture });
      drawInto(velocity.write);
      velocity.swap();

      for (const [field, fade, floorAtZero] of [
        [velocity, VELOCITY_FADE, 0],
        [dye, DYE_FADE, 1]
      ] as const) {
        const uniforms = use(advect, { velocity: velocity.read.texture, field: field.read.texture });
        gl.uniform1f(uniforms.get('dt') ?? null, dt);
        gl.uniform1f(uniforms.get('fade') ?? null, fade);
        gl.uniform1f(uniforms.get('floorAtZero') ?? null, floorAtZero);
        drawInto(field.write);
        field.swap();
      }
    };

    let pixelRatio = 1;
    const render = () => {
      if (!dye) return;
      const uniforms = use(display, { dye: dye.read.texture });
      gl.uniform2f(uniforms.get('resolution') ?? null, canvas.width, canvas.height);
      gl.uniform1f(uniforms.get('pixelRatio') ?? null, pixelRatio);
      gl.bindFramebuffer(gl.FRAMEBUFFER, null);
      gl.viewport(0, 0, canvas.width, canvas.height);
      gl.drawArrays(gl.TRIANGLES, 0, 3);
    };

    let animationFrame = 0;
    let lastFrame: number | null = null;
    let lastActivity = -Infinity;
    let onScreen = true;

    const tick = (now: number) => {
      const dt = lastFrame === null ? 1 / 60 : Math.min((now - lastFrame) / 1000, 1 / 30);
      lastFrame = now;
      colorCycle += dt * CYCLE_PER_SECOND;
      step(dt);
      render();
      if (now - lastActivity < IDLE_AFTER_MS && onScreen && !document.hidden) {
        animationFrame = requestAnimationFrame(tick);
      } else {
        animationFrame = 0;
        lastFrame = null;
      }
    };

    const wake = () => {
      lastActivity = performance.now();
      if (!animationFrame && onScreen && !document.hidden) animationFrame = requestAnimationFrame(tick);
    };

    const movePointer = (clientX: number, clientY: number) => {
      const rect = canvas.getBoundingClientRect();
      const inside = clientX >= rect.left && clientX <= rect.right && clientY >= rect.top && clientY <= rect.bottom;
      const now = performance.now();
      const x = ((clientX - rect.left) / rect.width) * gridWidth;
      const y = (1 - (clientY - rect.top) / rect.height) * gridHeight;
      const previous = pointer;
      pointer = { x, y, time: now };
      if (!inside || !previous || now - previous.time > 100) {
        smoothedVelocity = { x: 0, y: 0 };
        return;
      }
      const dx = x - previous.x;
      const dy = y - previous.y;
      const distance = Math.hypot(dx, dy);
      if (distance === 0 || distance > Math.max(gridWidth, gridHeight) * 0.3) return;
      const seconds = Math.max((now - previous.time) / 1000, 1 / 240);
      smoothedVelocity = {
        x: smoothedVelocity.x * 0.4 + (dx / seconds) * 0.6,
        y: smoothedVelocity.y * 0.4 + (dy / seconds) * 0.6
      };
      const count = Math.min(MAX_STAMPS, Math.max(1, Math.ceil(distance / STAMP_SPACING)));
      for (let index = 1; index <= count; index++) {
        const t = index / count;
        pendingStamps.push({
          x: previous.x + dx * t,
          y: previous.y + dy * t,
          impulseX: smoothedVelocity.x * IMPULSE,
          impulseY: smoothedVelocity.y * IMPULSE
        });
      }
      wake();
    };

    const onMouseMove = (event: MouseEvent) => movePointer(event.clientX, event.clientY);
    const onTouchMove = (event: TouchEvent) => {
      const touch = event.touches[0];
      if (touch) movePointer(touch.clientX, touch.clientY);
    };

    const resize = () => {
      pixelRatio = Math.min(devicePixelRatio, MAX_PIXEL_RATIO);
      canvas.width = Math.max(1, Math.round(canvas.clientWidth * pixelRatio));
      canvas.height = Math.max(1, Math.round(canvas.clientHeight * pixelRatio));
      if (!allocate(canvas.clientWidth, canvas.clientHeight)) {
        onUnavailable();
        return;
      }
      render();
    };

    const onVisibility = () => {
      if (!document.hidden) wake();
    };

    const resizeObserver = new ResizeObserver(resize);
    const intersectionObserver = new IntersectionObserver(([entry]) => {
      onScreen = entry?.isIntersecting ?? true;
      if (onScreen) wake();
    });

    resize();
    resizeObserver.observe(canvas);
    intersectionObserver.observe(canvas);
    window.addEventListener('mousemove', onMouseMove, { passive: true });
    window.addEventListener('touchmove', onTouchMove, { passive: true });
    document.addEventListener('visibilitychange', onVisibility);

    return () => {
      cancelAnimationFrame(animationFrame);
      resizeObserver.disconnect();
      intersectionObserver.disconnect();
      window.removeEventListener('mousemove', onMouseMove);
      window.removeEventListener('touchmove', onTouchMove);
      document.removeEventListener('visibilitychange', onVisibility);
    };
  }, []);

  return <canvas ref={canvasRef} aria-hidden="true" className="pointer-events-none absolute inset-0 -z-10 size-full" />;
}
