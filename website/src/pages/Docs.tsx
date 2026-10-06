import { Footer } from '../components/Footer';
import { Header } from '../components/Header';
import { INSTALL_GUIDE_URL, RAYCAST_URL, ZED_DOCS_URL } from '../consts';

const paths = [
  { label: 'Command line', path: 'wu' },
  { label: 'Settings (macOS, Linux)', path: '~/.config/wu' },
  { label: 'Settings (Windows)', path: '%APPDATA%\\Wu' },
  { label: 'App data (macOS)', path: '~/Library/Application Support/Wu' },
  { label: 'Project settings', path: '.wu/' }
];

export function Docs() {
  return (
    <>
      <Header />
      <main className="doc mx-auto max-w-[760px] px-8 pt-32 pb-16 max-sm:px-4">
        <h1>Docs</h1>

        <div className="mb-6 rounded-xl bg-elevated/5 px-5 py-[18px] text-[17px] leading-normal text-secondary [&_a]:text-primary [&_a]:underline [&_a]:underline-offset-2">
          This page covers what is specific to Wu. Wu shares its editor core with Zed, so for settings, key bindings,
          languages, extensions, tasks, debugging, and remote development, the{' '}
          <a href={ZED_DOCS_URL} target="_blank" rel="noopener">
            Zed documentation
          </a>{' '}
          applies to Wu as well.
        </div>

        <h2>Getting started</h2>
        <p>
          Download Wu for your platform and follow the{' '}
          <a href={INSTALL_GUIDE_URL} target="_blank" rel="noopener">
            install guide
          </a>
          .
        </p>
        <div className="overflow-x-auto">
          <table>
            <tbody>
              {paths.map((row) => (
                <tr key={row.label}>
                  <td>{row.label}</td>
                  <td>
                    <code>{row.path}</code>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>

        <h2>Agent Chat</h2>
        <p>
          Agent Chat lets you work with Claude Code, Codex or OpenCode inside Wu. It runs the <code>claude</code>,{' '}
          <code>codex</code> and <code>opencode</code> command line tools you already have installed and signed in, so
          there are no API keys and no Wu account.
        </p>
        <ul>
          <li>Open it from the agent icon in the activity bar, then start a new Claude, Codex or OpenCode chat.</li>
          <li>
            Type <code>/</code> for the agent's commands, <code>@</code> to mention a file, and <code>$</code> to use a
            skill.
          </li>
          <li>Paste, drop, or attach images to send them with a message.</li>
          <li>
            Messages you send while the agent is working wait in a queue. You can edit them, steer the current turn, or
            send one right away.
          </li>
          <li>Subagents open in their own tabs, and the agent's to-do list shows above the chat box.</li>
          <li>
            The model picker sets the model, reasoning level, fast mode, and how much the agent can do without asking.
            The rings next to it show how full the context is and, for Claude Code and Codex, your plan usage, where you
            can also switch accounts.
          </li>
          <li>Chats can be pinned, archived, sorted into sections, forked, or opened as side chats.</li>
          <li>
            Press <code>Cmd-D</code> (<code>Ctrl-D</code> on Windows and Linux) in the chat box to dictate. The speech
            model runs on your machine and downloads the first time you use it. Dictation works on macOS 14 or later
            with Apple Silicon, Linux, and Windows.
          </li>
          <li>Wu can play a sound and show a notification when the agent finishes, needs your input, or hits an error.</li>
        </ul>
        <p>
          The send key, sounds, notifications, dictation, and more are on the Agent Chat page in settings.
        </p>

        <h2>Workspace</h2>
        <ul>
          <li>
            The activity bar on the side switches between the project, git, outline, search, debug, and agent panels.
            The panels dock on the left by default, and <code>activity_bar.icon_size</code> sets the icon size.
          </li>
          <li>
            The <code>+</code> button in the editor and terminal tab strips sits after the last tab and opens a new file
            or terminal directly, with no menu.
          </li>
          <li>
            <code>Cmd-W</code> / <code>Ctrl-W</code> only closes editor tabs, never a side panel.
          </li>
        </ul>

        <h2>Extensions</h2>
        <p>Wu installs extensions from the Zed extension registry, and they work in Wu without changes.</p>

        <h2>Privacy</h2>
        <p>
          Wu has no account, no telemetry, and no crash reporting. It never sends your usage data anywhere. Agent Chat
          only talks to the providers behind the tools you use.
        </p>

        <h2>Not included</h2>
        <ul>
          <li>Collaboration: channels, calls, screen sharing, and shared projects.</li>
          <li>Vim and Helix modes.</li>
          <li>Dev containers.</li>
          <li>The REPL and the journal.</li>
        </ul>
        <p className="text-tertiary!">If a setting or key binding in the Zed docs belongs to one of these, it has no effect in Wu.</p>

        <h2>Coming from Zed</h2>
        <p>
          Wu and Zed can be installed side by side. They keep separate settings, so copy your <code>settings.json</code>{' '}
          and <code>keymap.json</code> from <code>~/.config/zed</code> to <code>~/.config/wu</code> if you want the
          same setup in both.
        </p>

        <h2>Raycast</h2>
        <p>
          On macOS, install the{' '}
          <a href={RAYCAST_URL} target="_blank" rel="noopener">
            Wu extension for Raycast
          </a>{' '}
          to open recent projects, new windows, and settings from Raycast.
        </p>

        <h2>License</h2>
        <p>Wu is distributed under GPL-3.0-or-later, with Apache-2.0 components where marked.</p>
      </main>
      <Footer />
    </>
  );
}
