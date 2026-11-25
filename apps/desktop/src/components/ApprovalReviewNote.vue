<script setup lang="ts">
import { NTag } from "naive-ui";
import type { ApprovalReview } from "../api";

defineProps<{ note: ApprovalReview }>();

const verdictLabels = {
  allow: { label: "AI 未发现额外风险", type: "success" },
  review: { label: "AI 建议人工确认", type: "warning" },
  deny: { label: "AI 建议拒绝", type: "error" },
  error: { label: "AI 审查降级", type: "warning" },
} as const;

const riskLabels = { low: "低风险", medium: "中风险", high: "高风险", critical: "严重风险" };
</script>

<template>
  <div class="review-note">
    <div class="review-tags">
      <n-tag size="small" :type="verdictLabels[note.verdict].type" :bordered="false">
        {{ verdictLabels[note.verdict].label }}
      </n-tag>
      <n-tag v-if="note.risk" size="small" :bordered="false">{{ riskLabels[note.risk] }}</n-tag>
      <span v-if="note.cached">缓存结论</span>
    </div>
    <span>{{ note.reason }}</span>
  </div>
</template>

<style scoped>
.review-note {
  display: flex;
  flex-direction: column;
  gap: 4px;
  font-size: 12px;
  line-height: 1.6;
  overflow-wrap: anywhere;
}
.review-tags {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 6px;
}
</style>
