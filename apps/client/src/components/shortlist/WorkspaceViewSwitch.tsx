// Rail switch between the Shortlist workspace, the Classic panel
// dashboard, the trade Journal and the panel Scorecard. Lives outside .app-main-controls on purpose: that group is
// hidden below the 1400px breakpoint, and the view switch must stay
// reachable at every width.

import { LayoutDashboard, ListOrdered, NotebookPen, Scale } from "lucide-react";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { isWorkspaceView, type WorkspaceView } from "../../lib/shortlist";

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
      onValueChange={(value) => { if (isWorkspaceView(value)) props.onChange(value); }}
      aria-label="Workspace view"
    >
      <ToggleGroupItem value="shortlist" className="app-rail-btn" aria-label="Shortlist view" title="Shortlist">
        <ListOrdered size={16} />
      </ToggleGroupItem>
      <ToggleGroupItem value="classic" className="app-rail-btn" aria-label="Classic panel view" title="Classic panels">
        <LayoutDashboard size={16} />
      </ToggleGroupItem>
      <ToggleGroupItem value="journal" className="app-rail-btn" aria-label="Trade journal" title="Journal">
        <NotebookPen size={16} />
      </ToggleGroupItem>
      <ToggleGroupItem value="scorecard" className="app-rail-btn" aria-label="Panel scorecard" title="Scorecard">
        <Scale size={16} />
      </ToggleGroupItem>
    </ToggleGroup>
  );
}
