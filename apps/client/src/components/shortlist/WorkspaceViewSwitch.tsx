// Rail switch between the Shortlist workspace and the Classic panel
// dashboard. Lives outside .app-main-controls on purpose: that group is
// hidden below the 1400px breakpoint, and the view switch must stay
// reachable at every width.

import { LayoutDashboard, ListOrdered } from "lucide-react";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import type { WorkspaceView } from "../../lib/shortlist";

export function WorkspaceViewSwitch(props: { view: WorkspaceView; onChange: (view: WorkspaceView) => void; horizontal?: boolean }) {
  return (
    <ToggleGroup
      type="single"
      orientation={props.horizontal ? "horizontal" : "vertical"}
      spacing={1}
      className="workspace-view-switch"
      value={props.view}
      // Radix lets a single-select group be cleared by re-clicking the
      // active item; there is always exactly one view, so ignore that.
      onValueChange={(value) => { if (value === "shortlist" || value === "classic") props.onChange(value); }}
      aria-label="Workspace view"
    >
      <ToggleGroupItem value="shortlist" className="app-rail-btn" aria-label="Shortlist view" title="Shortlist">
        <ListOrdered size={16} />
      </ToggleGroupItem>
      <ToggleGroupItem value="classic" className="app-rail-btn" aria-label="Classic panel view" title="Classic panels">
        <LayoutDashboard size={16} />
      </ToggleGroupItem>
    </ToggleGroup>
  );
}
