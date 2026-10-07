import { AppIcon } from './icons';
import { mediaFrame } from './Section';

const commands = ['Open Recent Project', 'Open with Wu', 'Open New Window', 'Open Settings', 'Open Keymap'];

const key = 'rounded-[0.6cqw] bg-white/10 px-[0.8cqw] py-[0.3cqw] font-system text-[1.7cqw] text-[#ececec]';

export function RaycastWindow() {
  return (
    <div className={`${mediaFrame} @container grid card-raised aspect-[16/10] place-items-center`} aria-hidden="true">
      <div className="w-[84%] overflow-hidden rounded-[1.6cqw] bg-[#1c1c1f] font-system text-[1.9cqw] tracking-normal text-[#ececec] shadow-[0_3cqw_8cqw_rgba(0,0,0,0.35),inset_0_0_0_1px_rgba(255,255,255,0.12)]">
        <div className="flex h-[8cqw] items-center border-b border-white/8 px-[2.6cqw] text-[2.6cqw]">
          wu
          <span className="ml-0.5 h-[3cqw] w-0.5 animate-blink bg-[#ececec] motion-reduce:animate-none" />
          <span className="ml-auto flex items-center gap-[1cqw] text-[2cqw] text-[#9a9aa1]">
            Ask AI
            <kbd className="rounded-[0.6cqw] px-[0.8cqw] py-[0.3cqw] font-system text-[1.7cqw] ring-1 ring-white/22 ring-inset">
              Tab
            </kbd>
          </span>
        </div>
        <div className="p-[1.2cqw]">
          <div className="px-[1.6cqw] pt-[0.4cqw] pb-[1cqw] text-[1.8cqw] text-[#9a9aa1]">Results</div>
          {commands.map((command, index) => (
            <div
              key={command}
              className={`flex h-[5.6cqw] items-center gap-[1.8cqw] rounded-[1cqw] px-[1.6cqw] ${index === 0 ? 'bg-white/9' : ''}`}
            >
              <AppIcon className="size-[3cqw] rounded-[24%]" />
              <span className="font-medium">{command}</span>
              <span className="text-[#9a9aa1]">Wu</span>
              <span className="ml-auto text-[#9a9aa1]">Command</span>
            </div>
          ))}
        </div>
        <div className="flex h-[6cqw] items-center justify-end gap-[1cqw] border-t border-white/8 bg-white/2.5 px-[2cqw] text-[1.8cqw] text-[#9a9aa1]">
          <span className="font-semibold text-[#ececec]">Open Command</span>
          <kbd className={key}>↵</kbd>
          <span className="mx-[1cqw] h-[2cqw] w-px bg-white/18" />
          Actions
          <kbd className={key}>⌘</kbd>
          <kbd className={key}>K</kbd>
        </div>
      </div>
    </div>
  );
}
