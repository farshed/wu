import { hydrateRoot } from 'react-dom/client';
import { Home } from '../pages/Home';

const root = document.getElementById('root');
if (root) hydrateRoot(root, <Home />);
