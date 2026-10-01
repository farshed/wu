import { Footer } from '../components/Footer';
import { Header } from '../components/Header';
import { sectionBody } from '../components/Section';

export function NotFound() {
  return (
    <>
      <Header />
      <main className="mx-auto flex max-w-[600px] flex-col items-center gap-4 px-8 pt-48 pb-16 text-center max-sm:px-4 max-sm:pt-36">
        <h1 className="text-[clamp(36px,5vw,48px)] leading-[1.1] font-medium tracking-[-0.03em]">Page not found</h1>
        <p className={sectionBody}>This page doesn't exist or has moved.</p>
        <a className="btn btn-md btn-primary mt-2" href="/">
          Back to Wu
        </a>
      </main>
      <Footer />
    </>
  );
}
