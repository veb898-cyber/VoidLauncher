import { useState } from 'react';
import { InstanceList } from '../instances/InstanceList';
import { InstanceDetail } from '../instances/InstanceDetail';
import { CreateInstanceWizard } from '../../pages/CreateInstanceWizard';

interface HomeLayoutProps {
  onNavigate: (page: string) => void;
}

export function HomeLayout({ onNavigate }: HomeLayoutProps) {
  const [showCreate, setShowCreate] = useState(false);

  return (
    <div style={{ display: 'flex', height: '100%' }}>
      <InstanceList onCreateClick={() => setShowCreate(true)} />
      <InstanceDetail onNavigate={onNavigate} />
      <CreateInstanceWizard open={showCreate} onClose={() => setShowCreate(false)} />
    </div>
  );
}
