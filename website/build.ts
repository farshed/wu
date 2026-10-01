import tailwind from 'bun-plugin-tailwind';
import { cp, rm } from 'node:fs/promises';
import path from 'node:path';
import { SITE_URL } from './src/consts';
import { renderPage } from './src/render';
import { routes } from './src/routes';

const root = import.meta.dir;
const outdir = path.join(root, 'dist');

await rm(outdir, { recursive: true, force: true });

const assets = await bundleAssets();
await prerenderRoutes(assets);
await writeSitemap();
await cp(path.join(root, 'public'), outdir, { recursive: true });

async function bundleAssets(): Promise<Map<string, string>> {
  const result = await Bun.build({
    entrypoints: [path.join(root, 'src/styles/index.css'), path.join(root, 'src/client/home.tsx')],
    outdir,
    plugins: [tailwind],
    external: ['/fonts/*'],
    splitting: true,
    minify: true,
    target: 'browser',
    naming: {
      entry: 'assets/[name]-[hash].[ext]',
      chunk: 'assets/chunk-[hash].[ext]',
      asset: 'assets/[name]-[hash].[ext]'
    },
    define: { 'process.env.NODE_ENV': JSON.stringify('production') }
  });

  // CSS entrypoints are reported as kind "asset", so "assets/index-ab12cd34.css" maps to "index".
  const hrefs = new Map<string, string>();
  for (const output of result.outputs) {
    const isEntry = output.kind === 'entry-point' || (output.kind === 'asset' && output.path.endsWith('.css'));
    if (!isEntry) continue;
    const base = path.basename(output.path);
    hrefs.set(base.slice(0, base.lastIndexOf('-')), '/' + path.relative(outdir, output.path).replaceAll(path.sep, '/'));
    logFile(output.path, output.size);
  }
  return hrefs;
}

async function prerenderRoutes(assets: Map<string, string>) {
  const css = assets.get('index');
  if (!css) throw new Error('Missing stylesheet bundle');

  for (const route of routes) {
    const js = route.entry ? assets.get(route.entry) : undefined;
    if (route.entry && !js) throw new Error(`Missing client bundle "${route.entry}" for ${route.path}`);

    const html = renderPage(route, { css, js });
    const file = outputPath(route.path);
    await Bun.write(file, html);
    logFile(file, html.length);
  }
}

function outputPath(routePath: string): string {
  if (routePath === '/') return path.join(outdir, 'index.html');
  if (routePath === '/404') return path.join(outdir, '404.html');
  return path.join(outdir, routePath.slice(1), 'index.html');
}

async function writeSitemap() {
  const urls = routes
    .map((route) => route.path)
    .filter((routePath) => routePath !== '/404')
    .map((routePath) => (routePath === '/' ? `${SITE_URL}/` : `${SITE_URL}${routePath}/`));

  const sitemap = `<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
${urls.map((url) => `  <url><loc>${url}</loc></url>`).join('\n')}
</urlset>
`;
  const file = path.join(outdir, 'sitemap.xml');
  await Bun.write(file, sitemap);
  logFile(file, sitemap.length);
}

function logFile(file: string, bytes: number) {
  console.log(` ${path.relative(root, file)}  ${(bytes / 1024).toFixed(1)} KB`);
}
