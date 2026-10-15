import { Liquid } from 'liquid-gooey';
import { useGooey } from '../../hooks/useGooey';

interface ToggleProps {
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
  /**
   * 'label' (default) is the whole 48×26 control. 'span' for rows already
   * wrapped in a <label> (duplicate-instance options): a nested label is
   * invalid HTML, and the outer one keeps forwarding clicks to the input.
   */
  as?: 'label' | 'span';
}

/**
 * The launcher's switch. The knob is a real element (was ::before) so the
 * gooey option can wrap it in a liquid group: the blob hides under the crisp
 * knob at rest and stretches into a trail while it slides.
 */
export function Toggle({ checked, onChange, disabled, as = 'label' }: ToggleProps) {
  const gooey = useGooey();
  const Tag = as;

  return (
    <Tag className="toggle">
      <input
        type="checkbox"
        checked={checked}
        disabled={disabled}
        onChange={(e) => onChange(e.target.checked)}
      />
      <span className="toggle__slider">
        {gooey ? (
          <Liquid
            fill="var(--text-primary)"
            style={{ position: 'absolute', inset: 0, pointerEvents: 'none' }}
          >
            {/* Default move tuning splits the small knob at speed: the trail
                droplet tears off behind it and the wobbling surface overshoots
                the crisp circle (owner: «от круглого ползунка … отрываются
                части»). Keep the liquid as a snug blurred coat instead. */}
            <Liquid.Item
              effect="move"
              move={{
                springiness: 0.9,
                wobble: 0.1,
                stretch: 0.1,
                trail: 0,
                advanced: { bend: 0, bendX: 0 },
              }}
            >
              <span className="toggle__knob" />
            </Liquid.Item>
          </Liquid>
        ) : (
          <span className="toggle__knob" />
        )}
      </span>
    </Tag>
  );
}
