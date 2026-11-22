export type Tab =
  | "overview"
  | "resume"
  | "preferences"
  | "jobs"
  | "applications"
  | "companies"
  | "extension"
  | "settings";

export type Skill = {
  name: string;
  evidence?: string[];
  confidence?: number;
};

export type ExperienceBullet = {
  text?: string;
  source_excerpt?: string;
};

export type Experience = {
  company?: string;
  title?: string;
  dates?: string;
  location?: string;
  bullets?: ExperienceBullet[];
};

export type Project = {
  name?: string;
  description?: string;
  technologies?: string[];
};

export type Education = {
  qualification?: string;
  institution?: string;
  dates?: string;
};

export type OpenSource = {
  project?: string;
  contribution?: string;
};

export type ResumeProfile = {
  uploaded?: boolean;
  name?: string;
  headline?: string;
  summary?: string;
  source_file?: string;
  skills?: Skill[];
  experience?: Experience[];
  projects?: Project[];
  education?: Education[];
  open_source?: OpenSource[];
  extraction_warnings?: string[];
  layout?: string[];
  custom_fields?: { id: string; label: string; body: string }[];
};

export type UploadResult = {
  skills: number;
  experience: number;
  projects: number;
  education: number;
  format: string;
  llm_configured: boolean;
};

export type Preferences = {
  name: string;
  summary: string;
  skills: string[];
  experience_years: number;
  preferred_titles: string[];
  role_keywords: string[];
  preferred_locations: string[];
  excluded_keywords: string[];
  minimum_match_score: number;
  projects: string[];
  preferred_work_modes: string[];
  auto_apply?: boolean;
  email?: string;
  phone?: string;
  skill_weights?: SkillWeight[];
};

export type SkillWeight = {
  skill: string;
  weight: number;
};

export type Job = {
  external_id: string;
  source: string;
  title: string;
  company: string;
  location?: string | null;
  description: string;
  skills?: string[];
  url: string;
  published_at?: string | null;
  work_mode?: string | null;
};

export type JobResult = {
  job: Job;
  score: number;
  status?: string;
  matched_skills?: string[];
  missing_skills?: string[];
  explanation?: string;
  rank?: number;
};

export type PagedJobs = {
  total: number;
  page: number;
  per_page: number;
  jobs: JobResult[];
};

export type TailorResult = {
  relevant_experience_bullets?: string[];
  integrity_note?: string;
};

export type TailoredResumeResult = {
  headline: string;
  summary: string;
  skills: string[];
  markdown?: string;
  html?: string;
  style?: string;
  matched_skills?: string[];
  missing_requirements?: string[];
  llm_used?: boolean;
  llm_provider?: string | null;
  integrity_note?: string;
  roles?: unknown[];
  projects?: unknown[];
};

export type PrepQuestion = {
  question: string;
  talking_points: string[];
};

export type PrepSheetResult = {
  overview: string;
  technical_questions: PrepQuestion[];
  behavioral_questions: PrepQuestion[];
  questions_to_ask: string[];
  generated_by?: string | null;
};

export type CompanyEntry = {
  key: string;
  name: string;
  jobs: number;
  applications: number;
  top_role: string;
  top_score: number;
  statuses: string[];
  has_guide: boolean;
  notes_preview: string;
};

export type StudyTopic = {
  topic: string;
  priority: string;
  why: string;
};

export type StudyComponent = {
  name: string;
  notes: string;
};

export type CompanyQA = {
  question: string;
  answer: string;
  topic: string;
  source_repo: string;
  source_license: string;
  source_url: string;
  generated?: boolean;
};

export type PrimerLink = {
  title: string;
  url: string;
};

export type StudyGuide = {
  company: string;
  roles: string[];
  top_skills: string[];
  seniority: string;
  applications: number;
  dsa_topics: StudyTopic[];
  system_design: StudyComponent[];
  capacity_notes: string;
  resume_tips: string[];
  community_qa?: CompanyQA[];
  primer_links?: PrimerLink[];
  dsa_lists?: PrimerLink[];
  lld_practice?: string[];
  lld_links?: PrimerLink[];
  generated_by?: string | null;
  integrity_note: string;
};

export type CompanyGuideResult = {
  company: string;
  notes: string;
  guide: StudyGuide;
  cached: boolean;
};

export type Application = {
  id: string;
  external_id: string;
  job_title: string;
  company: string;
  job_url: string;
  status: string;
  tailored_summary: string;
  selected_bullets: string[];
  notes?: string;
};

export type Source = {
  url: string;
  name?: string | null;
};

export type GreenhouseBoard = {
  token: string;
  name?: string | null;
};

export type SearchConfig = {
  target_country: string;
  max_age_days: number;
  location: string;
  extra_locations: string[];
  include_remote: boolean;
  include_hybrid: boolean;
  follow_location: boolean;
  detected_city: string;
  detected_region: string;
  detected_country: string;
  latitude?: number | null;
  longitude?: number | null;
  nearby: string[];
  location_source: string;
};

export type DiscoveryStatus = {
  interval_hours: number;
  last_run: {
    finished_at?: string | null;
    discovered?: number;
    error?: string | null;
  } | null;
  provider_count: number;
  onnx_enabled: boolean;
  serpapi_configured: boolean;
  jsearch_configured: boolean;
  tavily_configured: boolean;
  brave_configured: boolean;
  serpapi_hint?: string | null;
  jsearch_hint?: string | null;
  tavily_hint?: string | null;
  brave_hint?: string | null;
  greenhouse_boards: GreenhouseBoard[];
};
