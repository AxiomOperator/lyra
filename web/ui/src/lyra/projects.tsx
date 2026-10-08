// Projects: folders on this PC lent to lyra. They stay in this browser;
// lyra reads them while lyra is open here, and asks before changing a file.

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { CheckCheck, FolderOpen, FolderPlus, Pencil, ShieldCheck, Trash2 } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { Back } from "./manage";
import { Page, useConfirm } from "./parts";
import { addFolder, allowFolder, folderStates, onFoldersChanged, removeFolder, renameFolder, supported, trustFolder, type FolderState } from "./folders";

export function ProjectsPage({ onBack }: { onBack: () => void }) {
  const [folders, setFolders] = useState<FolderState[]>([]);
  const [renaming, setRenaming] = useState<{ name: string; to: string } | null>(null);
  const [confirm, dialog] = useConfirm();
  const reload = useCallback(() => void folderStates().then(setFolders), []);
  useEffect(() => {
    reload();
    return onFoldersChanged(reload);
  }, [reload]);
  return (
    <Page
      title="Projects"
      description={'Folders on this PC that lyra may work in. Ask "what\'s in my Firewall folder?" or "add the meeting notes to notes.md". lyra reads them while it\'s open here and asks before changing anything.'}
      action={<Back onBack={onBack} />}
    >
      {!supported && (
        <Card className="border-amber-700/50 py-3">
          <CardContent className="px-4 text-sm">This browser can't lend folders. Use Chrome or Edge (or lyra installed from them) on your PC.</CardContent>
        </Card>
      )}
      {supported && (
        <div>
          <Button onClick={() => void addFolder()}>
            <FolderPlus /> Open folder…
          </Button>
        </div>
      )}
      <Card className="gap-0 py-3">
        <CardHeader className="px-4">
          <CardTitle className="text-sm">On this PC</CardTitle>
          <CardDescription>Only you can reach these, and only from this browser.</CardDescription>
        </CardHeader>
        <CardContent className="divide-y px-4">
          {folders.length === 0 && <div className="py-2 text-muted-foreground text-sm">No folders yet.</div>}
          {folders.map((f) => (
            <div key={f.name} className="flex flex-wrap items-center gap-2 py-2">
              <FolderOpen className="size-4 text-muted-foreground" />
              {renaming?.name === f.name ? (
                <form
                  className="flex flex-1 gap-2"
                  onSubmit={(e) => {
                    e.preventDefault();
                    void renameFolder(f.name, renaming.to).then((ok) => ok && setRenaming(null));
                  }}
                >
                  <Input autoFocus value={renaming.to} onChange={(e) => setRenaming({ name: f.name, to: e.target.value })} />
                  <Button size="sm" type="submit">
                    Save
                  </Button>
                </form>
              ) : (
                <span className="min-w-0 flex-1 truncate text-sm">{f.name}</span>
              )}
              {f.allowed ? <Badge variant="secondary">{f.writable ? "read & change" : "read only"}</Badge> : <Badge variant="destructive">needs your OK</Badge>}
              {f.allowed && f.writable && (
                <Button
                  size="sm"
                  variant={f.trusted ? "secondary" : "outline"}
                  title={f.trusted ? "lyra changes files here without asking: tap to be asked again" : "Let lyra change files here without asking each time"}
                  onClick={() =>
                    f.trusted
                      ? void trustFolder(f.name, false)
                      : confirm({ title: `Let lyra change files in ${f.name} without asking?`, text: "It still can't delete anything or reach outside this folder, and only your own conversations can use it. Turn this off any time.", action: "Always allow", run: () => void trustFolder(f.name, true) })
                  }
                >
                  <CheckCheck /> {f.trusted ? "Always allowed" : "Always allow"}
                </Button>
              )}
              {!f.allowed || !f.writable ? (
                <Button size="sm" onClick={() => void allowFolder(f.name)}>
                  <ShieldCheck /> Allow
                </Button>
              ) : null}
              <Button size="icon" variant="ghost" aria-label="Rename" onClick={() => setRenaming({ name: f.name, to: f.name })}>
                <Pencil />
              </Button>
              <Button
                size="icon"
                variant="ghost"
                aria-label="Remove"
                onClick={() => confirm({ title: `Stop lending ${f.name}?`, text: "lyra won't see it any more. Nothing in the folder changes.", action: "Remove", run: () => void removeFolder(f.name) })}
              >
                <Trash2 />
              </Button>
            </div>
          ))}
        </CardContent>
      </Card>
      {dialog}
    </Page>
  );
}
