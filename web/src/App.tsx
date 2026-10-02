import { useEffect } from "react";

import { MetricsPanel } from "./components/MetricsPanel";
import { SearchPalette } from "./components/SearchPalette";
import { StoryDrawer } from "./components/StoryDrawer";
import { StoryFeed } from "./components/StoryFeed";
import { Timeline } from "./components/Timeline";
import { TopBar } from "./components/TopBar";
import { GraphCanvas } from "./graph/GraphCanvas";
import { Legend } from "./graph/Legend";
import { useLiveStream } from "./live/stream";
import { useApp } from "./live/store";

export function App() {
  useLiveStream();
  const theme = useApp((s) => s.theme);
  const selected = useApp((s) => s.selected);
  const feedOpen = useApp((s) => s.feedOpen);

  useEffect(() => {
    const root = document.documentElement;
    if (theme === "system") root.removeAttribute("data-theme");
    else root.setAttribute("data-theme", theme);
  }, [theme]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        useApp.getState().setSearchOpen(true);
      } else if (e.key === "/" && !(e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement)) {
        e.preventDefault();
        useApp.getState().setSearchOpen(true);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <div className={`app ${selected ? "has-drawer" : ""}`}>
      <a className="skip-link" href="#stories">
        Skip to story list
      </a>
      <TopBar />
      <div id="stories" className="app__rail">
        <StoryFeed />
      </div>
      {feedOpen && <div className="scrim" onClick={() => useApp.getState().toggleFeed(false)} aria-hidden />}
      <main className="app__main">
        <GraphCanvas />
        <Legend />
        <MetricsPanel />
      </main>
      <StoryDrawer />
      <Timeline />
      <SearchPalette />
    </div>
  );
}
