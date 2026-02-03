import { registerRoot, Composition } from 'remotion';
import { ParallelAgentsHero } from './Composition';
import { timing } from './styles/colors';

const RemotionRoot: React.FC = () => {
  return (
    <>
      {/* 16:9 for YouTube/LinkedIn articles */}
      <Composition
        id="ParallelAgentsHero"
        component={ParallelAgentsHero}
        durationInFrames={timing.totalFrames}
        fps={timing.fps}
        width={1280}
        height={720}
      />

      {/* Square for LinkedIn feed posts */}
      <Composition
        id="ParallelAgentsSquare"
        component={ParallelAgentsHero}
        durationInFrames={timing.totalFrames}
        fps={timing.fps}
        width={1080}
        height={1080}
      />

      {/* Vertical for stories */}
      <Composition
        id="ParallelAgentsVertical"
        component={ParallelAgentsHero}
        durationInFrames={timing.totalFrames}
        fps={timing.fps}
        width={1080}
        height={1920}
      />
    </>
  );
};

registerRoot(RemotionRoot);
