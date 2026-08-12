import { WiredAppIcon } from './Brand';

/**
 * What he sees before he has said anything.
 *
 * The old copy pitched the product to a developer — "drive them over HTTP all
 * day — from a script, a cron job or your phone". The question a first-time
 * user actually has is *what can I ask it, right now?*, so the answer is three
 * things he can click.
 */

const STARTERS = [
  {
    title: 'Get to know it',
    prompt: 'Introduce yourself. What can you do, and what can you see on this computer?',
  },
  {
    title: 'Tidy something up',
    prompt: 'Look through the files in my folder and tell me what is in there, in plain English.',
  },
  {
    title: 'Do it every day',
    prompt: 'Write me a short summary of anything new in my folder since yesterday.',
  },
];

interface WelcomeProps {
  onStart: (prompt: string) => void;
  assistantName: string;
}

export function Welcome({ onStart, assistantName }: WelcomeProps) {
  return (
    <div className="welcome fade-in">
      <WiredAppIcon size={48} />
      <h2>Your assistant is ready</h2>
      <p>
        Ask {assistantName} for something in your own words. It will keep working while this window
        is closed, and you can reach it from your phone.
      </p>

      <div className="starter-grid">
        {STARTERS.map((starter) => (
          <button
            key={starter.title}
            type="button"
            className="starter-card"
            onClick={() => onStart(starter.prompt)}
          >
            <span className="name">{starter.title}</span>
            <span className="blurb">{starter.prompt}</span>
          </button>
        ))}
      </div>
    </div>
  );
}
