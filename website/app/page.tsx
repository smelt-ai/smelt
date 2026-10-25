import { Nav } from "./components/Nav";
import { Hero } from "./components/Hero";
import { Features } from "./components/Features";
import { Showcase } from "./components/Showcase";
import { Integrations } from "./components/Integrations";
import { Cta } from "./components/Cta";
import { Footer } from "./components/Footer";

export default function Home() {
  return (
    <div className="flex flex-1 flex-col bg-background">
      <Nav />
      <main className="flex-1">
        <Hero />
        <Features />
        <Showcase />
        <Integrations />
        <Cta />
      </main>
      <Footer />
    </div>
  );
}
