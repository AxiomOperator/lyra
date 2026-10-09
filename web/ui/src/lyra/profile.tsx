// Your profile: who you are and how lyra reaches you, in a few tabs —
// account (password, devices), connections (Outlook, PMI), email from lyra,
// this device's notifications, and privacy (what lyra knows about you).

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { cn } from "@/lib/utils";
import { Bell, Brain, Link2, Mail, ShieldCheck, UserRound } from "lucide-react";
import { useCallback, useEffect, useState, type ReactNode } from "react";
import { ConnectedAccounts, EmailCard, PasswordCard, YourDevices, type Me } from "./aboutme";
import { CalendarCard } from "./calendar";
import { Notifications } from "./pages";
import { Back, Failed, Page, useAction, useConfirm } from "./parts";
import { useLyra } from "./store";

export interface Section {
  id: string;
  label: string;
  icon?: ReactNode;
}

/** A page's sections as tabs: a row on phones, a column beside the content on wider screens. */
export function SectionTabs({ sections, current, pick, children }: { sections: Section[]; current: string; pick: (id: string) => void; children: ReactNode }) {
  return (
    <div className="flex flex-col gap-4 md:flex-row md:items-start">
      <nav className="-mx-1 flex shrink-0 gap-1 overflow-x-auto px-1 pb-1 md:sticky md:top-2 md:w-44 md:flex-col md:overflow-visible" aria-label="Sections">
        {sections.map((s) => (
          <button
            key={s.id}
            type="button"
            onClick={() => pick(s.id)}
            aria-current={s.id === current ? "page" : undefined}
            className={cn(
              "flex shrink-0 items-center gap-2 whitespace-nowrap rounded-md px-3 py-1.5 text-left text-sm",
              s.id === current ? "bg-accent font-medium text-foreground" : "text-muted-foreground hover:bg-accent/50 hover:text-foreground",
            )}
          >
            {s.icon}
            {s.label}
          </button>
        ))}
      </nav>
      <div className="min-w-0 flex-1 space-y-4">{children}</div>
    </div>
  );
}

const SECTIONS: Section[] = [
  { id: "account", label: "Account", icon: <UserRound className="size-4" /> },
  { id: "connections", label: "Connections", icon: <Link2 className="size-4" /> },
  { id: "email", label: "Email", icon: <Mail className="size-4" /> },
  { id: "notifications", label: "Notifications", icon: <Bell className="size-4" /> },
  { id: "privacy", label: "Privacy", icon: <ShieldCheck className="size-4" /> },
];

/** Back from connecting Outlook: the Connections tab. */
function firstSection() {
  if (/connect(ed|-error)=/.test(location.hash)) return "connections";
  try {
    return localStorage.getItem("lyra-profile-tab") ?? "account";
  } catch {
    return "account";
  }
}

export function ProfilePage({ onBack, toAboutMe }: { onBack: () => void; toAboutMe: () => void }) {
  const { call, ready, user } = useLyra();
  const [me, setMe] = useState<Me | null>(null);
  const load = useCallback(() => void call<Me>("me").then(setMe), [call]);
  const { act, note } = useAction(load);
  const [confirm, dialog] = useConfirm();
  const [tab, setTab] = useState(firstSection);
  useEffect(() => {
    if (ready) load();
  }, [ready, load]);
  const pick = (id: string) => {
    setTab(id);
    try {
      localStorage.setItem("lyra-profile-tab", id);
    } catch {
      // just not remembered
    }
  };
  return (
    <Page title="Profile" description="Who you are here, and how lyra reaches you." action={<Back onBack={onBack} />}>
      <Failed error={me?.error} />
      {note}
      <SectionTabs sections={SECTIONS} current={tab} pick={pick}>
        {tab === "account" && (
          <>
            <Card className="gap-1 py-4">
              <CardHeader className="px-4">
                <CardTitle className="flex flex-wrap items-center gap-2 text-base">
                  {me?.name ?? user?.name ?? "You"}
                  <Badge variant="outline">{me?.admin ? "admin" : "member"}</Badge>
                </CardTitle>
                <CardDescription className="space-y-0.5">
                  {me?.email && <span className="block">Email: {me.email}</span>}
                  {me?.username && <span className="block">Signs in as {me.username}</span>}
                  {!me?.email && !me?.username && <span className="block">Signs in with a pairing code.</span>}
                </CardDescription>
              </CardHeader>
            </Card>
            {me?.username && <PasswordCard username={me.username} />}
            <YourDevices me={me} />
          </>
        )}
        {tab === "connections" && (
          <>
            <CalendarCard />
            <ConnectedAccounts me={me} act={act} confirm={confirm} />
          </>
        )}
        {tab === "email" && <EmailCard />}
        {tab === "notifications" && <Notifications />}
        {tab === "privacy" && (
          <Card className="gap-1 py-4">
            <CardHeader className="px-4">
              <CardTitle className="flex items-center gap-2 text-base">
                <Brain className="size-4" /> What lyra knows about you
              </CardTitle>
              <CardDescription>Your memories, how you write, and everything lyra did for you with its Why?: correct, delete or export any of it.</CardDescription>
            </CardHeader>
            <CardContent className="px-4">
              <Button size="sm" variant="secondary" onClick={toAboutMe}>
                Open it
              </Button>
            </CardContent>
          </Card>
        )}
      </SectionTabs>
      {dialog}
    </Page>
  );
}
