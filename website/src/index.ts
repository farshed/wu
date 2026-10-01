import { serve } from 'bun';
import tailwind from 'bun-plugin-tailwind';
import path from 'node:path';
import { renderPage } from './render';
import { routes } from './routes';

const publicDir = path.join(import.meta.dir, '..', 'public');

let clientAssets = new Map<string, Blob>();

async function buildClient() {
  const result = await Bun.build({
    entrypoints: [path.join(import.meta.dir, 'styles', 'index.css'), path.join(import.meta.dir, 'client', 'home.tsx')],
    plugins: [tailwind],
    external: ['/fonts/*'],
    splitting: true,
    target: 'browser',
    naming: { entry: '[name].[ext]', chunk: 'chunk-[hash].[ext]', asset: '[name]-[hash].[ext]' }
  });
  clientAssets = new Map(result.outputs.map((output) => ['/' + path.basename(output.path), output]));
}

async function servePage(pathname: string): Promise<Response> {
  const route = routes.find((candidate) => candidate.path === pathname) ?? routes.find((candidate) => candidate.path === '/404');
  if (!route) return new Response('Not found', { status: 404 });

  await buildClient();
  const html = renderPage(route, { css: '/index.css', js: route.entry ? `/${route.entry}.js` : undefined });
  return new Response(html, {
    status: route.path === '/404' ? 404 : 200,
    headers: { 'Content-Type': 'text/html; charset=utf-8' }
  });
}

const server = serve({
  routes: {
    '/*': async (request) => {
      let { pathname } = new URL(request.url);
      if (pathname !== '/' && pathname.endsWith('/')) pathname = pathname.slice(0, -1);

      const asset = clientAssets.get(pathname);
      if (asset) return new Response(asset);

      const file = Bun.file(path.join(publicDir, pathname));
      if (await file.exists()) return new Response(file);

      return servePage(pathname);
    }
  }
});

console.log(`Wu website running at ${server.url}`);
