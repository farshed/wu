import { ArrowUpRight, BookOpen, Download } from 'lucide-react';
import { useEffect, useState } from 'react';
import { Faq } from '../components/Faq';
import { Footer } from '../components/Footer';
import { Header } from '../components/Header';
import { AppIcon, GithubIcon } from '../components/icons';
import { HeroBackground } from '../components/HeroBackground';
import { MemoryChart } from '../components/MemoryChart';
import { RaycastWindow } from '../components/RaycastWindow';
import { CenterSection, MediaSection, frameOutline, mediaFrame, sectionBody, sectionTitle } from '../components/Section';
import { WuWindowFrame } from '../components/WuWindow';
import { INSTALL_GUIDE_URL, MEMORY_BENCHMARK_URL, RAYCAST_URL, RELEASES_URL, REPO_URL } from '../consts';
import { detectPlatform, directDownload, downloads, platformNames, type Platform } from '../data/downloads';

export function Home() {
  const [platform, setPlatform] = useState<Platform | null>(null);

  useEffect(() => setPlatform(detectPlatform()), []);

  const direct = directDownload(platform);
  const downloadHref = direct?.href ?? '#download';

  return (
    <>
      <Header downloadHref={downloadHref} />
      <main>
        <div className="relative isolate m-1 mb-40 overflow-hidden rounded-xl max-sm:mb-18">
          <div
            aria-hidden="true"
            className="absolute inset-0 -z-10 bg-linear-to-b from-hero-start to-hero-end"
            style={{ maskImage: 'linear-gradient(to bottom, black 40%, transparent)' }}
          >
            <HeroBackground />
          </div>
          <section className="px-8 pt-42 pb-26 max-sm:px-4 max-sm:pt-30 max-sm:pb-14">
            <div className="mx-auto flex max-w-[1240px] flex-col items-start gap-5">
              <h1 className="max-w-[800px] text-[clamp(36px,5.2vw,60px)] leading-[1.1] font-medium tracking-[-0.03em]">
                The fast, native code&nbsp;editor
              </h1>
              <p className="max-w-[600px] text-xl leading-[1.35] max-sm:text-lg">
                Written in Rust. Batteries included: coding agents, a beautiful design and everything you need from day
                one.
              </p>
              <div className="mt-2 flex flex-wrap gap-2">
                <a className="btn btn-lg btn-light" href={downloadHref}>
                  <Download size={18} aria-hidden="true" />
                  {direct && platform ? `Download for ${platformNames[platform]}` : 'Download Wu'}
                </a>
                <a className="btn btn-lg btn-primary" href="#download">
                  All platforms
                </a>
              </div>
            </div>
          </section>

          <section className="mx-auto mt-28 max-w-[1240px] px-8 max-sm:mt-18 max-sm:px-4">
            <WuWindowFrame className={`${mediaFrame} ${frameOutline}`} />
          </section>
        </div>

        <MediaSection
          mediaFirst
          text={
            <>
              <h2 className={sectionTitle}>Light on memory</h2>
              <p className={sectionBody}>Wu used less memory than Zed and VS Code in every test we ran.</p>
              <p className={sectionBody}>
                Sitting idle, it uses about half as much as VS Code. Searching a whole repository, it uses 58% less
                than Zed.
              </p>
              <a className="btn btn-md btn-light mt-2" href={MEMORY_BENCHMARK_URL} target="_blank" rel="noopener">
                See benchmark
                <ArrowUpRight size={18} aria-hidden="true" />
              </a>
            </>
          }
          media={<MemoryChart />}
        />

        <MediaSection
          text={
            <>
              <h2 className={sectionTitle}>Wu, from Raycast</h2>
              <p className={sectionBody}>
                On macOS, the Wu extension for Raycast opens recent projects, new windows, your settings and more,
                without reaching for the mouse.
              </p>
              <a className="btn btn-md btn-soft mt-2" href={RAYCAST_URL} target="_blank" rel="noopener">
                Get the extension
                <ArrowUpRight size={18} aria-hidden="true" />
              </a>
            </>
          }
          media={<RaycastWindow />}
        />

        <CenterSection>
          <h2 className={sectionTitle}>Open source, built on Zed</h2>
          <p className={sectionBody}>
            Wu is a fork of Zed. It keeps Zed's editor core, GPU rendering and language tooling, and every Zed
            extension works in Wu.
          </p>
          <div className="mt-2 flex flex-wrap justify-center gap-2">
            <a className="btn btn-md btn-light" href={REPO_URL} target="_blank" rel="noopener">
              <GithubIcon />
              Check out the source
            </a>
            <a className="btn btn-md btn-primary" href="/docs/">
              <BookOpen size={18} aria-hidden="true" />
              Read the docs
            </a>
          </div>
        </CenterSection>

        <CenterSection id="download" card>
          <AppIcon className="mb-2 size-24 rounded-[24%]" alt="Wu app icon" />
          <h2 className={sectionTitle}>Ready when you are</h2>
          <p className={`${sectionBody} max-w-[520px]`}>Wu is free and takes a minute to install. Pick your platform.</p>
          <div className="mt-2 flex flex-wrap justify-center gap-2">
            {downloads.map((download) => (
              <a
                key={download.label}
                className={`btn btn-md ${download.platform && download.platform === platform ? 'btn-primary' : 'btn-soft'}`}
                href={download.href}
              >
                {download.label}
              </a>
            ))}
          </div>
          <p className="mt-2 text-sm leading-[1.4] text-tertiary">
            First time on macOS or Linux? See the{' '}
            <a href={INSTALL_GUIDE_URL} target="_blank" rel="noopener">
              install guide
            </a>
            .
          </p>
        </CenterSection>

        <Faq />
      </main>
      <Footer />
    </>
  );
}
