import { MicrosoftLoginCard } from '../components/MicrosoftLoginCard';

interface LoginProps {
  onNavigate: (page: string) => void;
}

export function Login({ onNavigate }: LoginProps) {
  return (
    <div className="login-page animate-fade-in">
      <MicrosoftLoginCard onSuccess={() => onNavigate('home')} />
    </div>
  );
}
