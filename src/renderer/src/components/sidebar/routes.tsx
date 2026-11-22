import {
  AppsIcon,
  DownloadIcon,
  BookIcon,
  HomeIcon,
  PeopleIcon,
} from "@primer/octicons-react";

// Each page has its own icon colour (palette blues, gold, teal, slate, soft pink and error red;
// no green, orange or purple). Labels sit in tooltips, so colour only helps scanning.
export const routes = [
  {
    path: "/",
    tint: "#adc6ff",
    nameKey: "home",
    render: () => <HomeIcon />,
  },
  {
    path: "/catalogue",
    tint: "#e8c77d",
    nameKey: "catalogue",
    render: () => <AppsIcon />,
  },
  {
    path: "/library",
    tint: "#4fd1b0",
    nameKey: "library",
    render: () => <BookIcon />,
  },
  {
    path: "/downloads",
    tint: "#7fa7e0",
    nameKey: "downloads",
    render: () => <DownloadIcon />,
  },
  {
    path: "/friends",
    tint: "#f2a7c3",
    nameKey: "friends",
    render: () => <PeopleIcon />,
    requiresAuth: false,
  },
];
