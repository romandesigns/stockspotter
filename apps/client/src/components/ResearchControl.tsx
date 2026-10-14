import { FlaskConical } from "lucide-react";
import { Button } from "./ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "./ui/popover";
import { Switch } from "./ui/switch";

export function ResearchControl(props: { enabled: boolean; onChange: (enabled: boolean) => void; saved: boolean; error: string | null }) {
  return <Popover><PopoverTrigger asChild>
    <Button variant="ghost" size="icon" className="app-rail-btn" aria-label="Research panels" title={props.error ? "Research panels: connection refused" : !props.saved ? "Research panels: choice not saved" : "Research panels"}><FlaskConical size={16} /></Button>
  </PopoverTrigger><PopoverContent side="right" align="start" className="w-80 space-y-3">
    <p className="font-semibold">Research panels</p>
    <label className="flex items-center justify-between gap-3" htmlFor="research-panels-enabled"><span>IAC and Extreme Momentum</span>
      <Switch id="research-panels-enabled" checked={props.enabled} onCheckedChange={props.onChange} />
    </label>
    <p className="text-sm text-muted-foreground">Observations from the available feed. These panels do not change alerts, rankings or trading. You can switch them off at any time.</p>
    {!props.saved && <p role="status" className="text-sm">This choice applies now, but could not be saved for the next launch.</p>}
    {props.enabled && props.error && <p role="status" className="text-sm">Research feed: {props.error}. Switch off and on to retry after resolving it.</p>}
  </PopoverContent></Popover>;
}
