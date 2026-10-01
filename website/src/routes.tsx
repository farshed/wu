import type { ReactElement } from 'react';
import { Docs } from './pages/Docs';
import { Home } from './pages/Home';
import { NotFound } from './pages/NotFound';

export type ClientEntry = 'home';

export interface Route {
  path: string;
  element: ReactElement;
  entry?: ClientEntry;
  meta?: {
    title?: string;
    description?: string;
  };
}

export const routes: Route[] = [
  { path: '/', element: <Home />, entry: 'home', meta: { title: 'Wu: the fast, native code editor' } },
  {
    path: '/docs',
    element: <Docs />,
    meta: { title: 'Wu Docs', description: 'How Wu differs from Zed: what is removed, what is different, and where things live.' }
  },
  { path: '/404', element: <NotFound />, meta: { title: 'Page not found · Wu' } }
];
