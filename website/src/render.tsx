import { renderToStaticMarkup, renderToString } from 'react-dom/server';
import { SITE_DESCRIPTION, SITE_TITLE, SITE_URL } from './consts';
import type { Route } from './routes';

interface Assets {
  css: string;
  js?: string;
}

const escapeHtml = (text: string) =>
  text.replaceAll('&', '&amp;').replaceAll('"', '&quot;').replaceAll('<', '&lt;');

export function renderPage(route: Route, { css, js }: Assets): string {
  const title = escapeHtml(route.meta?.title ?? SITE_TITLE);
  const description = escapeHtml(route.meta?.description ?? SITE_DESCRIPTION);
  const url = `${SITE_URL}${route.path === '/' ? '/' : `${route.path}/`}`;
  const image = `${SITE_URL}/screenshot-dark.png`;
  const app = js ? renderToString(route.element) : renderToStaticMarkup(route.element);

  return `<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width,initial-scale=1" />
    <title>${title}</title>
    <meta name="description" content="${description}" />
    <link rel="canonical" href="${url}" />
    <link rel="icon" href="/app-icon.png" />
    <meta property="og:type" content="website" />
    <meta property="og:site_name" content="${SITE_TITLE}" />
    <meta property="og:url" content="${url}" />
    <meta property="og:title" content="${title}" />
    <meta property="og:description" content="${description}" />
    <meta property="og:image" content="${image}" />
    <meta name="twitter:card" content="summary_large_image" />
    <meta name="twitter:image" content="${image}" />
    <meta name="theme-color" content="#06040a" />
    <script>document.documentElement.classList.add('js')</script>
    <link rel="preload" href="/fonts/ibm-plex-sans-latin.woff2" as="font" type="font/woff2" crossorigin />
    <link rel="stylesheet" href="${css}" />
${js ? `    <script type="module" src="${js}"></script>\n` : ''}  </head>
  <body>
    <div id="root">${app}</div>
  </body>
</html>
`;
}
