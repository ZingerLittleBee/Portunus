import { apiFetch } from "@/api/client";

export interface AuthStatus {
  onboarding_required: boolean;
}

export interface LoginRequest {
  user_id: string;
  password: string;
}

export interface LoginResponse {
  password_change_required: boolean;
}

export interface OnboardingRequest {
  user_id: string;
  display_name: string;
  password: string;
  password_confirm: string;
  setup_token: string;
}

export function getAuthStatus(): Promise<AuthStatus> {
  return apiFetch<AuthStatus>("/v1/auth/status");
}

export function login(body: LoginRequest): Promise<LoginResponse> {
  return apiFetch<LoginResponse>("/v1/auth/login", {
    method: "POST",
    body: JSON.stringify(body),
  });
}

export function logout(): Promise<void> {
  return apiFetch<void>("/v1/auth/logout", { method: "POST" });
}

export function onboard(body: OnboardingRequest): Promise<void> {
  return apiFetch<void>("/v1/auth/onboarding", {
    method: "POST",
    body: JSON.stringify(body),
  });
}
