export interface MemoryScenario {
  title: string;
  results: { editor: string; mib: number }[];
}

export const memoryScenarios: MemoryScenario[] = [
  {
    title: 'Sitting idle',
    results: [
      { editor: 'Wu', mib: 382.5 },
      { editor: 'Zed', mib: 420.4 },
      { editor: 'VS Code', mib: 743.5 }
    ]
  },
  {
    title: 'Searching a repository',
    results: [
      { editor: 'Wu', mib: 582.9 },
      { editor: 'Zed', mib: 1395.3 },
      { editor: 'VS Code', mib: 823.6 }
    ]
  }
];
