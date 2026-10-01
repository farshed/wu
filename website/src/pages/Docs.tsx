import { Footer } from '../components/Footer';
import { Header } from '../components/Header';
import { RAYCAST_URL, ZED_DOCS_URL, ZED_URL } from '../consts';

const paths = [
  { label: 'Command line', zed: 'zed', wu: 'wu' },
  { label: 'Settings (macOS, Linux)', zed: '~/.config/zed', wu: '~/.config/wu' },
  { label: 'Settings (Windows)', zed: '%APPDATA%\\Zed', wu: '%APPDATA%\\Wu' },
  { label: 'App data (macOS)', zed: '~/Library/Application Support/Zed', wu: '~/Library/Application Support/Wu' },
  { label: 'Project settings', zed: '.zed/', wu: '.wu/' }
];

export function Docs() {
  return (
    <>
      <Header />
      <main className="doc mx-auto max-w-[760px] px-8 pt-32 pb-16 max-sm:px-4">
        <h1>Docs</h1>

        <div className="mb-6 rounded-xl bg-elevated/5 px-5 py-[18px] text-[17px] leading-normal text-secondary [&_a]:text-primary [&_a]:underline [&_a]:underline-offset-2">
          Wu is a fork of{' '}
          <a href={ZED_URL} target="_blank" rel="noopener">
            Zed
          </a>
          . Everything that is not listed on this page works the way it does in Zed, so the{' '}
          <a href={ZED_DOCS_URL} target="_blank" rel="noopener">
            Zed documentation
          </a>{' '}
          is the reference for settings, key bindings, languages, extensions, tasks, debugging, and remote development.
        </div>

        <h2>What is removed</h2>
        <ul>
          <li>AI features: the agent panel, inline assistant, edit predictions, and language model providers.</li>
          <li>Collaboration: channels, calls, screen sharing, shared projects, and the sign-in flow that went with them.</li>
          <li>Telemetry, crash reporting, and hang detection. Wu does not phone home.</li>
          <li>Vim and Helix modes.</li>
          <li>Dev containers.</li>
          <li>The REPL and the journal.</li>
        </ul>
        <p className="text-tertiary!">
          If a setting or key binding from the Zed docs belongs to one of these features, it has no effect in Wu.
        </p>

        <h2>What is different</h2>
        <ul>
          <li>
            An activity bar on the side switches between the project, git, outline, search, and debug panels. The panels
            dock on the left by default.
          </li>
          <li>
            The <code>+</code> button in the editor and terminal tab strips sits after the last tab and opens a new file
            or terminal directly, with no menu.
          </li>
          <li>
            <code>Cmd-W</code> / <code>Ctrl-W</code> never closes a side panel. It only closes editor tabs.
          </li>
          <li>Icons and the UI use less memory than Zed for the same workload.</li>
        </ul>

        <h2>Names and paths</h2>
        <div className="overflow-x-auto">
          <table>
            <thead>
              <tr>
                <th />
                <th>Zed</th>
                <th>Wu</th>
              </tr>
            </thead>
            <tbody>
              {paths.map((row) => (
                <tr key={row.label}>
                  <td>{row.label}</td>
                  <td>
                    <code>{row.zed}</code>
                  </td>
                  <td>
                    <code>{row.wu}</code>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
        <p>
          Wu and Zed can be installed side by side. They do not share settings, so copy your <code>settings.json</code>{' '}
          and <code>keymap.json</code> over if you want the same setup in both.
        </p>

        <h2>What is the same</h2>
        <ul>
          <li>Extensions come from the Zed extension registry, and every Zed extension works in Wu.</li>
          <li>
            Settings, key bindings, themes, and languages follow the{' '}
            <a href={ZED_DOCS_URL} target="_blank" rel="noopener">
              Zed docs
            </a>
            .
          </li>
        </ul>

        <h2>Raycast</h2>
        <p>
          On macOS, install the{' '}
          <a href={RAYCAST_URL} target="_blank" rel="noopener">
            Wu extension for Raycast
          </a>{' '}
          to control Wu from Raycast.
        </p>

        <h2>License</h2>
        <p>Wu is distributed under the same terms as Zed: GPL-3.0-or-later, with Apache-2.0 components where marked.</p>
      </main>
      <Footer />
    </>
  );
}
